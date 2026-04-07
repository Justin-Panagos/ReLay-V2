pub mod chunked;
pub mod lifecycle;

use crate::db::{self, DbState};
use crate::shield;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tauri::Manager;
use tokio_util::sync::CancellationToken;

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

/// Payload emitted when a download fails or is cancelled.
#[derive(Clone, serde::Serialize)]
pub struct ErrorPayload {
    /// Human-readable error description.
    pub message: String,
}

/// Payload emitted when a download is paused.
#[derive(Clone, serde::Serialize)]
pub struct PausedPayload {}

/// Entry point for a fresh download. Registers the token/intent via the caller before spawning.
///
/// Orchestrates the full lifecycle: download → outcome handling → queue drain.
///
/// Args:
///   url:         The HTTP/HTTPS URL to download from.
///   destination: The directory path to save the file into.
///   filename:    The filename to use on disk.
///   id:          The downloads table row id.
///   app:         Tauri app handle.
///   token:       CancellationToken from the lifecycle registry.
///   intent:      Intent flag (NONE/PAUSE/CANCEL) — set by pause/cancel commands.
pub async fn download_file(
    url: String,
    destination: String,
    filename: String,
    id: i64,
    app: tauri::AppHandle,
    token: CancellationToken,
    intent: Arc<AtomicU8>,
) {
    let dest_path = PathBuf::from(&destination);
    let file_path = dest_path.join(&filename);

    let outcome = run_download(&url, &destination, &filename, id, &app, token).await;
    handle_outcome(outcome, intent, id, &file_path, &url, &filename, &app).await;

    // Clean up lifecycle registry and drain the queue.
    if let Some(lifecycle) = app.try_state::<lifecycle::LifecycleState>() {
        lifecycle::deregister_download(&lifecycle, id);
    }
    lifecycle::try_start_next(app);
}

/// Entry point for resuming a previously paused download.
/// Loads chunk snapshots from DB and continues from the saved byte offsets.
///
/// Args:
///   id:     The downloads table row id to resume.
///   app:    Tauri app handle.
///   token:  Fresh CancellationToken from the lifecycle registry.
///   intent: Intent flag — set by pause/cancel commands.
pub async fn download_file_resume(
    id: i64,
    app: tauri::AppHandle,
    token: CancellationToken,
    intent: Arc<AtomicU8>,
) {
    // Load record and snapshots from DB.
    let (record, snapshots) = {
        let db = app.try_state::<DbState>();
        let Some(db) = db else { return };
        let conn = db.0.lock().unwrap();
        let record = match db::get_download_by_id(&conn, id) {
            Ok(Some(r)) => r,
            _ => return,
        };
        let snapshots = db::load_chunk_snapshots(&conn, id).unwrap_or_default();
        (record, snapshots)
    };

    let file_path = PathBuf::from(&record.destination).join(&record.filename);

    // Mark as downloading again.
    if let Some(db) = app.try_state::<DbState>() {
        if let Ok(conn) = db.0.lock() {
            db::update_download_status(&conn, id, "downloading").ok();
        }
    }

    let url = record.url.clone();
    let filename = record.filename.clone();

    let outcome = run_download_resume(
        &url,
        &record.destination,
        &filename,
        id,
        &app,
        token,
        snapshots,
    )
    .await;
    handle_outcome(outcome, intent, id, &file_path, &url, &filename, &app).await;

    if let Some(lifecycle) = app.try_state::<lifecycle::LifecycleState>() {
        lifecycle::deregister_download(&lifecycle, id);
    }
    lifecycle::try_start_next(app);
}

/// Handles a `LifecycleOutcome` (or error) from `run_download` / `run_download_resume`:
/// runs the Shield scan pipeline on completion, updates DB, and emits frontend events.
///
/// Args:
///   outcome:   The result from the download runner.
///   intent:    Intent flag to distinguish pause from cancel when outcome is Paused.
///   id:        The download row id.
///   file_path: Full path to the (possibly partial) file on disk.
///   url:       The original download URL (passed to Shield Layer 5).
///   filename:  The file name (passed to Shield for quarantine records).
///   app:       Tauri app handle.
async fn handle_outcome(
    outcome: Result<chunked::LifecycleOutcome, String>,
    intent: Arc<AtomicU8>,
    id: i64,
    file_path: &PathBuf,
    url: &str,
    filename: &str,
    app: &tauri::AppHandle,
) {
    match outcome {
        Ok(chunked::LifecycleOutcome::Complete) => {
            // Emit scan-start event — triggers amber pulse on the download card.
            app.emit_all(
                &format!("download://scan/{id}"),
                shield::ScanPayload {
                    layer: 0,
                    name: "Starting scan".to_string(),
                },
            )
            .ok();
            if let Some(db) = app.try_state::<DbState>() {
                if let Ok(conn) = db.0.lock() {
                    db::update_download_status(&conn, id, "scanning").ok();
                }
            }

            // Run the six-layer pipeline.
            let result = shield::run_pipeline(file_path, url, filename, id, app).await;

            // Persist SHA-256 regardless of verdict.
            if let Some(db) = app.try_state::<DbState>() {
                if let Ok(conn) = db.0.lock() {
                    db::update_download_sha256(&conn, id, &result.sha256).ok();
                }
            }

            // Persist sandbox report if Layer 7 ran.
            if let Some(ref report_json) = result.sandbox_report {
                if let Some(db) = app.try_state::<DbState>() {
                    if let Ok(conn) = db.0.lock() {
                        db::update_sandbox_report(&conn, id, report_json).ok();
                    }
                }
            }

            match result.verdict {
                shield::FinalVerdict::Clean => {
                    if let Some(db) = app.try_state::<DbState>() {
                        if let Ok(conn) = db.0.lock() {
                            db::update_download_status(&conn, id, "complete").ok();
                            db::delete_chunk_snapshots(&conn, id).ok();
                        }
                    }
                    app.emit_all(
                        &format!("download://complete/{id}"),
                        CompletePayload {
                            path: file_path.to_string_lossy().to_string(),
                        },
                    )
                    .ok();
                }
                shield::FinalVerdict::Quarantined { reason } => {
                    let quar_path = quarantine_path(file_path);
                    if let Some(parent) = quar_path.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    std::fs::rename(file_path, &quar_path).ok();

                    if let Some(db) = app.try_state::<DbState>() {
                        if let Ok(conn) = db.0.lock() {
                            db::update_download_status(&conn, id, "quarantined").ok();
                            db::update_download_scan_threat(&conn, id, &reason).ok();
                            db::delete_chunk_snapshots(&conn, id).ok();
                            db::insert_quarantine(
                                &conn,
                                id,
                                &result.sha256,
                                filename,
                                &file_path.to_string_lossy(),
                                &quar_path.to_string_lossy(),
                                &reason,
                            )
                            .ok();
                        }
                    }
                    app.emit_all(
                        &format!("download://quarantine/{id}"),
                        shield::QuarantinePayload {
                            reason,
                            sha256: result.sha256,
                        },
                    )
                    .ok();
                }
            }
        }
        Ok(chunked::LifecycleOutcome::Paused(snapshots)) => {
            let intent_val = intent.load(Ordering::Relaxed);
            if intent_val == lifecycle::intent::PAUSE {
                if let Some(db) = app.try_state::<DbState>() {
                    if let Ok(conn) = db.0.lock() {
                        db::save_chunk_snapshots(&conn, id, &snapshots).ok();
                        db::update_download_status(&conn, id, "paused").ok();
                    }
                }
                app.emit_all(&format!("download://paused/{id}"), PausedPayload {})
                    .ok();
            } else {
                // Cancel (or any other non-pause intent): delete file and mark failed.
                std::fs::remove_file(file_path).ok();
                if let Some(db) = app.try_state::<DbState>() {
                    if let Ok(conn) = db.0.lock() {
                        db::delete_chunk_snapshots(&conn, id).ok();
                        db::update_download_status(&conn, id, "failed").ok();
                    }
                }
                app.emit_all(
                    &format!("download://error/{id}"),
                    ErrorPayload {
                        message: "Cancelled".to_string(),
                    },
                )
                .ok();
            }
        }
        Err(e) => {
            if let Some(db) = app.try_state::<DbState>() {
                if let Ok(conn) = db.0.lock() {
                    db::delete_chunk_snapshots(&conn, id).ok();
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
}

/// Orchestrates a fresh download: GET probe → decide strategy → execute.
///
/// Args:
///   url:         The HTTP/HTTPS URL to download from.
///   destination: The directory path to save the file into.
///   filename:    The filename to use on disk.
///   id:          The downloads table row id.
///   app:         Tauri app handle.
///   token:       CancellationToken for pause/cancel.
///
/// Returns:
///   Ok(LifecycleOutcome) on clean finish or pause. Err(message) on failure.
async fn run_download(
    url: &str,
    destination: &str,
    filename: &str,
    id: i64,
    app: &tauri::AppHandle,
    token: CancellationToken,
) -> Result<chunked::LifecycleOutcome, String> {
    let dest_path = PathBuf::from(destination);
    std::fs::create_dir_all(&dest_path).map_err(|e| e.to_string())?;
    let file_path = dest_path.join(filename);

    let client = app
        .try_state::<reqwest::Client>()
        .map(|s| s.inner().clone())
        .unwrap_or_else(reqwest::Client::new);

    // Mark as downloading in DB.
    if let Some(state) = app.try_state::<DbState>() {
        if let Ok(conn) = state.0.lock() {
            db::update_download_status(&conn, id, "downloading").ok();
        }
    }

    // Single GET probe — avoids a separate HEAD round-trip that some servers handle slowly.
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

    // Store known file size in DB immediately so the history tab can show it.
    if let Some(size) = content_length {
        if let Some(state) = app.try_state::<DbState>() {
            if let Ok(conn) = state.0.lock() {
                db::update_download_size(&conn, id, size).ok();
            }
        }
    }

    // Pro users get 16 parallel chunks; Free users get 8.
    let max_chunks = if app
        .try_state::<DbState>()
        .and_then(|db| db.0.lock().ok().map(|conn| crate::pro::is_pro(&conn)))
        .unwrap_or(false)
    {
        chunked::PRO_TIER_CHUNKS
    } else {
        chunked::FREE_TIER_CHUNKS
    };

    // Use chunked only when the file is large enough to benefit from parallel chunks.
    let chunk_count = content_length
        .map(|n| {
            (n / chunked::MIN_CHUNK_BYTES).clamp(1, max_chunks as u64) as usize
        })
        .unwrap_or(1);

    if accepts_ranges && chunk_count > 1 {
        chunked::download_chunked(
            url.to_string(),
            file_path,
            content_length.unwrap(),
            response,
            id,
            app.clone(),
            client,
            token,
            None,
        )
        .await
    } else {
        download_single(response, &file_path, content_length, id, app, token).await
    }
}

/// Orchestrates a resume download: reuses chunk snapshots to skip already-written data.
///
/// Args:
///   url:         The HTTP/HTTPS URL to download from.
///   destination: The directory path.
///   filename:    The filename.
///   id:          The download row id.
///   app:         Tauri app handle.
///   token:       Fresh CancellationToken.
///   snapshots:   Per-chunk byte-offset snapshots from the paused session.
///
/// Returns:
///   Ok(LifecycleOutcome) on clean finish or pause. Err(message) on failure.
async fn run_download_resume(
    url: &str,
    destination: &str,
    filename: &str,
    id: i64,
    app: &tauri::AppHandle,
    token: CancellationToken,
    snapshots: Vec<crate::db::ChunkSnapshot>,
) -> Result<chunked::LifecycleOutcome, String> {
    let dest_path = PathBuf::from(destination);
    let file_path = dest_path.join(filename);

    let client = app
        .try_state::<reqwest::Client>()
        .map(|s| s.inner().clone())
        .unwrap_or_else(reqwest::Client::new);

    if snapshots.is_empty() {
        // No chunk snapshots — fall back to a full fresh download.
        return run_download(url, destination, filename, id, app, token).await;
    }

    // Determine total_size from the last snapshot's end_byte.
    let total_size = snapshots
        .iter()
        .map(|s| s.end_byte + 1)
        .max()
        .unwrap_or(0);

    if total_size == 0 {
        return run_download(url, destination, filename, id, app, token).await;
    }

    // Use a placeholder probe response: send a Range request for byte 0-0 as the "probe".
    // This is the simplest approach — chunked resume for each chunk handles its own Range.
    // We need to open a connection for chunk 0; use a small range request.
    let probe_response = client
        .get(url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !probe_response.status().is_success() && probe_response.status().as_u16() != 206 {
        return Err(format!("HTTP {} on resume probe", probe_response.status()));
    }

    // Pass resume_offsets to the chunked engine; it adjusts each chunk's start byte.
    chunked::download_chunked(
        url.to_string(),
        file_path,
        total_size,
        probe_response,
        id,
        app.clone(),
        client,
        token,
        Some(snapshots),
    )
    .await
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
///   token:    CancellationToken for pause/cancel.
///
/// Returns:
///   Ok(LifecycleOutcome) on success or pause. Err(message) on any failure.
async fn download_single(
    mut response: reqwest::Response,
    path: &PathBuf,
    total: Option<u64>,
    id: i64,
    app: &tauri::AppHandle,
    token: CancellationToken,
) -> Result<chunked::LifecycleOutcome, String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    let mut downloaded: u64 = 0;
    let mut last_bytes: u64 = 0;
    let mut last_tick = std::time::Instant::now();

    loop {
        let maybe_chunk = tokio::select! {
            result = response.chunk() => result.map_err(|e| e.to_string())?,
            _ = token.cancelled() => {
                writer.flush().ok();
                // Single-stream pause: emit a single-element snapshot so resume can Range-seek.
                let snapshot = vec![crate::db::ChunkSnapshot {
                    chunk_idx: 0,
                    start_byte: 0,
                    end_byte: total.unwrap_or(downloaded).saturating_sub(1),
                    written_bytes: downloaded,
                }];
                return Ok(chunked::LifecycleOutcome::Paused(snapshot));
            }
        };

        let chunk = match maybe_chunk {
            Some(c) => c,
            None => break,
        };

        writer.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        // Rate-limit events to ~5/s to match the chunked path's reporter cadence.
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
                ProgressPayload { downloaded, total, speed_bps },
            )
            .ok();
        }
    }

    writer.flush().map_err(|e| e.to_string())?;

    // Emit final 100% event so the bar reaches 100% before the complete event hides it.
    if let Some(total_size) = total {
        app.emit_all(
            &format!("download://progress/{id}"),
            ProgressPayload { downloaded: total_size, total, speed_bps: 0 },
        )
        .ok();
    }

    Ok(chunked::LifecycleOutcome::Complete)
}

/// Returns the quarantine path for a file: `{data_dir}/ReLay/quarantine/{filename}`.
/// Falls back to `./quarantine/{filename}` if the OS data directory is unavailable.
///
/// Args:
///   file_path: The file's original path (filename is extracted from it).
///
/// Returns:
///   A PathBuf pointing to where the quarantined file should be stored.
fn quarantine_path(file_path: &std::path::Path) -> PathBuf {
    let filename = file_path
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("quarantined"));
    let base = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ReLay")
        .join("quarantine");
    base.join(filename)
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
