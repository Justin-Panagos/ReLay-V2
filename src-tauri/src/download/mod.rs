pub mod chunked;

use crate::db::{self, DbState};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use tauri::Manager;

/// Progress payload emitted to the frontend on each reporter tick.
#[derive(Clone, serde::Serialize)]
pub struct ProgressPayload {
    /// Total bytes received so far across all chunks.
    pub downloaded: u64,
    /// Total file size in bytes, if the server sent Content-Length.
    pub total: Option<u64>,
    /// Current download speed in bytes per second.
    pub speed_bps: u64,
}

/// Payload emitted when a download completes successfully.
#[derive(Clone, serde::Serialize)]
pub struct CompletePayload {
    /// Absolute path to the downloaded file on disk.
    pub path: String,
}

/// Payload emitted when a download fails.
#[derive(Clone, serde::Serialize)]
pub struct ErrorPayload {
    /// Human-readable error description.
    pub message: String,
}

/// Downloads a file from `url` and saves it to `destination/filename`.
/// Dispatches to the chunked engine or single-stream fallback based on
/// server capability. Emits Tauri events and updates the database.
///
/// Args:
///   url:         The HTTP/HTTPS URL to download from.
///   destination: The directory path to save the file into.
///   filename:    The filename to use on disk.
///   id:          The downloads table row id for DB status updates.
///   app:         Tauri app handle — used for event emission and DB state access.
pub async fn download_file(
    url: String,
    destination: String,
    filename: String,
    id: i64,
    app: tauri::AppHandle,
) {
    let result = run_download(&url, &destination, &filename, id, &app).await;

    if let Err(e) = result {
        if let Some(state) = app.try_state::<DbState>() {
            if let Ok(conn) = state.0.lock() {
                db::update_download_status(&conn, id, "failed").ok();
            }
        }
        app.emit_all(
            &format!("download://error/{id}"),
            ErrorPayload { message: e },
        )
        .ok();
    }
}

/// Orchestrates the full download lifecycle: GET probe → decide strategy → execute → DB update.
///
/// Args:
///   url:         The HTTP/HTTPS URL to download from.
///   destination: The directory path to save the file into.
///   filename:    The filename to use on disk.
///   id:          The downloads table row id.
///   app:         Tauri app handle.
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
async fn run_download(
    url: &str,
    destination: &str,
    filename: &str,
    id: i64,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let dest_path = PathBuf::from(destination);
    std::fs::create_dir_all(&dest_path).map_err(|e| e.to_string())?;
    let file_path = dest_path.join(filename);

    let client = app
        .try_state::<reqwest::Client>()
        .map(|s| s.inner().clone())
        .unwrap_or_else(reqwest::Client::new);

    // Mark as downloading in DB
    if let Some(state) = app.try_state::<DbState>() {
        if let Ok(conn) = state.0.lock() {
            db::update_download_status(&conn, id, "downloading").ok();
        }
    }

    // Single GET probe — avoids a separate HEAD round-trip that some servers handle slowly
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let content_length = response.content_length();
    let accepts_ranges = response
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("bytes"))
        .unwrap_or(false);

    // Store known file size in DB immediately so the history tab can show it
    if let Some(size) = content_length {
        if let Some(state) = app.try_state::<DbState>() {
            if let Ok(conn) = state.0.lock() {
                db::update_download_size(&conn, id, size).ok();
            }
        }
    }

    // Use chunked only when the file is large enough to benefit from parallel chunks
    let chunk_count = content_length
        .map(|n| {
            (n / chunked::MIN_CHUNK_BYTES)
                .clamp(1, chunked::FREE_TIER_CHUNKS as u64) as usize
        })
        .unwrap_or(1);

    if accepts_ranges && chunk_count > 1 {
        // Pass the probe response into the chunked engine — it becomes chunk 0.
        // No connection teardown or reconnect needed for the first chunk.
        chunked::download_chunked(
            url.to_string(),
            file_path.clone(),
            content_length.unwrap(),
            response,
            id,
            app.clone(),
            client,
        )
        .await?;
    } else {
        download_single(response, &file_path, content_length, id, app).await?;
    }

    // Mark complete in DB
    if let Some(state) = app.try_state::<DbState>() {
        if let Ok(conn) = state.0.lock() {
            db::update_download_status(&conn, id, "complete").ok();
        }
    }

    app.emit_all(
        &format!("download://complete/{id}"),
        CompletePayload {
            path: file_path.to_string_lossy().to_string(),
        },
    )
    .ok();

    Ok(())
}

/// Single-connection streaming download. Used when the server does not support range requests
/// or when the file is too small to benefit from parallel chunks. The caller supplies the
/// already-open GET response so no second round-trip is needed.
///
/// Args:
///   response: An in-flight GET response from the probe request.
///   path:     Full destination file path.
///   total:    Content-Length if known from the response headers, None otherwise.
///   id:       The downloads table row id.
///   app:      Tauri app handle.
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
async fn download_single(
    mut response: reqwest::Response,
    path: &PathBuf,
    total: Option<u64>,
    id: i64,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let effective_total = total;

    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    let mut downloaded: u64 = 0;
    let mut last_bytes: u64 = 0;
    let mut last_tick = std::time::Instant::now();

    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        writer.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        // Rate-limit events to ~5/s to match the chunked path's reporter cadence
        if last_tick.elapsed().as_millis() >= 200 {
            let elapsed = last_tick.elapsed().as_secs_f64();
            let speed_bps = if elapsed > 0.0 {
                ((downloaded - last_bytes) as f64 / elapsed) as u64
            } else {
                0
            };
            last_bytes = downloaded;
            last_tick = std::time::Instant::now();

            app.emit_all(
                &format!("download://progress/{id}"),
                ProgressPayload { downloaded, total: effective_total, speed_bps },
            )
            .ok();
        }
    }

    writer.flush().map_err(|e| e.to_string())?;

    // Emit final 100% event so the bar reaches 100% before the complete event hides it
    if let Some(total_size) = effective_total {
        app.emit_all(
            &format!("download://progress/{id}"),
            ProgressPayload { downloaded: total_size, total: effective_total, speed_bps: 0 },
        )
        .ok();
    }

    Ok(())
}

/// Extracts a filename from a URL by taking the last path segment
/// and stripping any query string. Falls back to `"download"`.
///
/// Args:
///   url: The URL string to parse.
///
/// Returns:
///   A filename string suitable for use on disk.
pub fn extract_filename(url: &str) -> String {
    url.split('/')
        .next_back()
        .and_then(|s| s.split('?').next())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("download")
        .to_string()
}
