use crate::db::{self, DbState};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use tauri::Manager;

/// Progress payload emitted to the frontend on each received chunk.
#[derive(Clone, serde::Serialize)]
pub struct ProgressPayload {
    /// Total bytes received so far.
    pub downloaded: u64,
    /// Total file size in bytes, if the server sent Content-Length.
    pub total: Option<u64>,
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
/// Emits Tauri events as the download progresses and on completion or failure.
/// Updates the downloads table in SQLite to reflect current status.
/// Accesses the database through the AppHandle's managed state.
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
        if let Ok(state) = app.try_state::<DbState>().ok_or(()) {
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

/// Inner download logic that returns a Result so errors propagate cleanly.
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

    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let total = response.content_length();

    // Mark as downloading in DB
    if let Some(state) = app.try_state::<DbState>() {
        if let Ok(conn) = state.0.lock() {
            db::update_download_status(&conn, id, "downloading").ok();
        }
    }

    let file = std::fs::File::create(&file_path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    let mut downloaded: u64 = 0;
    let mut response = response;

    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        writer.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        app.emit_all(
            &format!("download://progress/{id}"),
            ProgressPayload { downloaded, total },
        )
        .ok();
    }

    writer.flush().map_err(|e| e.to_string())?;

    // Mark as complete in DB
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
