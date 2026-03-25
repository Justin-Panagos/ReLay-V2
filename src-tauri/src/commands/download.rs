use crate::db::{self, DbState};
use crate::download;
use tauri::State;

/// Starts a file download from the given URL.
/// Reads the destination folder from the `default_folder` setting,
/// inserts a DB row, spawns a background download task, and returns
/// the download id immediately so the frontend can subscribe to events.
///
/// Args:
///   url:   The HTTP/HTTPS URL to download.
///   state: Tauri-managed database state.
///   app:   Tauri app handle passed to the background task for event emission.
///
/// Returns:
///   Ok(id) — the downloads table row id — on success.
///   Err(message) if settings or DB insert fails.
#[tauri::command]
pub async fn start_download(
    url: String,
    state: State<'_, DbState>,
    app: tauri::AppHandle,
) -> Result<i64, String> {
    let filename = download::extract_filename(&url);

    // Read default_folder — lock, read, unlock immediately
    let destination = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "default_folder")
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| ".".to_string())
    };

    // Insert download row — lock, insert, unlock immediately
    let id = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        db::insert_download(&conn, &url, &filename, &destination)
            .map_err(|e| e.to_string())?
    };

    // Spawn background task — AppHandle is Clone + Send so it crosses the thread boundary safely
    tauri::async_runtime::spawn(async move {
        download::download_file(url, destination, filename, id, app).await;
    });

    Ok(id)
}

/// Returns recent downloads from the database, ordered by created_at descending.
///
/// Args:
///   limit: Maximum number of rows to return.
///   state: Tauri managed database state.
///
/// Returns:
///   Vec of DownloadRecord on success, or an error string.
#[tauri::command]
pub fn get_downloads(
    limit: i64,
    state: State<'_, DbState>,
) -> Result<Vec<db::DownloadRecord>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::get_recent_downloads(&conn, limit).map_err(|e| e.to_string())
}

/// Marks any downloads with status 'downloading' or 'queued' as 'failed'.
/// Called on app startup to clean up downloads that were interrupted by a
/// previous close or crash and cannot be resumed in this phase.
///
/// Args:
///   state: Tauri managed database state.
///
/// Returns:
///   Ok(()) on success, or an error string.
#[tauri::command]
pub fn reset_stale_downloads(state: State<'_, DbState>) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::reset_stale_downloads(&conn).map_err(|e| e.to_string())
}
