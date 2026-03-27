use crate::db::{self, DbState};
use crate::download::{self, lifecycle};
use std::sync::atomic::Ordering;
use tauri::{Manager, State};

/// Starts a file download from the given URL.
/// If a download slot is free (free tier: 1 active), the task starts immediately.
/// Otherwise the id is pushed onto the FIFO QueueState and the task will auto-start
/// when the current download finishes, is paused, or is cancelled.
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

    // Read default_folder — lock, read, unlock immediately.
    let destination = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "default_folder")
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| ".".to_string())
    };

    // Insert download row — lock, insert, unlock immediately.
    let id = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        db::insert_download(&conn, &url, &filename, &destination)
            .map_err(|e| e.to_string())?
    };

    // Check whether a slot is free by looking at the lifecycle registry.
    // Free tier: max 1 active download (registry is empty when no download is running).
    let slot_free = {
        let lifecycle = app.state::<lifecycle::LifecycleState>();
        let empty = lifecycle.0.lock().unwrap().is_empty();
        empty
    };

    if slot_free {
        let lifecycle = app.state::<lifecycle::LifecycleState>();
        let (token, intent) = lifecycle::register_download(&lifecycle, id);
        let app2 = app.clone();
        tokio::spawn(download::download_file(
            url, destination, filename, id, app2, token, intent,
        ));
    } else {
        let queue = app.state::<lifecycle::QueueState>();
        queue.0.lock().unwrap().push_back(id);
    }

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
/// previous close or crash.
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

/// Pauses an active download by setting the intent flag to PAUSE and firing
/// the cancellation token. The background task will yield cleanly, save
/// chunk snapshots to the database, and emit `download://paused/{id}`.
///
/// Args:
///   id:        The download row id.
///   lifecycle: Tauri-managed lifecycle state.
///
/// Returns:
///   Ok(()) if a handle was found and cancelled, Err if the download is not active.
#[tauri::command]
pub fn pause_download(
    id: i64,
    lifecycle: State<'_, lifecycle::LifecycleState>,
) -> Result<(), String> {
    let guard = lifecycle.0.lock().unwrap();
    if let Some(handle) = guard.get(&id) {
        handle.intent.store(lifecycle::intent::PAUSE, Ordering::Relaxed);
        handle.token.cancel();
        Ok(())
    } else {
        Err(format!("No active download with id {id}"))
    }
}

/// Cancels a download regardless of its current state (active, queued, or paused).
///
/// - Active: fires the cancellation token; the background task handles cleanup.
/// - Queued: removes the id from QueueState; no task is running.
/// - Paused: no task is running; cleans up directly.
///
/// For queued and paused downloads, this function deletes the partial file (if any),
/// removes chunk snapshots, sets DB status to `failed`, and emits `download://error/{id}`.
///
/// Args:
///   id:        The download row id.
///   lifecycle: Tauri-managed lifecycle state.
///   queue:     Tauri-managed queue state.
///   db:        Tauri-managed database state.
///   app:       Tauri app handle (for emitting the error event).
///
/// Returns:
///   Ok(()) always — cancel is best-effort.
#[tauri::command]
pub fn cancel_download(
    id: i64,
    lifecycle: State<'_, lifecycle::LifecycleState>,
    queue: State<'_, lifecycle::QueueState>,
    db: State<'_, DbState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Case 1: actively downloading — fire token, task handles all cleanup.
    {
        let guard = lifecycle.0.lock().unwrap();
        if let Some(handle) = guard.get(&id) {
            handle.intent.store(lifecycle::intent::CANCEL, Ordering::Relaxed);
            handle.token.cancel();
            return Ok(());
        }
    }

    // Case 2: in the FIFO queue — remove it so it never starts.
    {
        let mut q = queue.0.lock().unwrap();
        if let Some(pos) = q.iter().position(|&x| x == id) {
            q.remove(pos);
        }
    }

    // Case 3: queued (no file yet) or paused (partial file on disk) — clean up here.
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        if let Ok(Some(record)) = db::get_download_by_id(&conn, id) {
            let file_path = std::path::PathBuf::from(&record.destination)
                .join(&record.filename);
            std::fs::remove_file(&file_path).ok();
            db::delete_chunk_snapshots(&conn, id).ok();
            db::update_download_status(&conn, id, "failed").ok();
        }
    }
    app.emit_all(
        &format!("download://error/{id}"),
        download::ErrorPayload {
            message: "Cancelled".to_string(),
        },
    )
    .ok();
    Ok(())
}

/// Returns a single download record by its row id.
///
/// Args:
///   id:    The downloads table row id.
///   db:    Tauri-managed database state.
///
/// Returns:
///   Some(DownloadRecord) if found, None if not found, Err on DB error.
#[tauri::command]
pub fn get_download_by_id(
    id: i64,
    db: State<'_, DbState>,
) -> Result<Option<db::DownloadRecord>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::get_download_by_id(&conn, id).map_err(|e| e.to_string())
}

/// Resumes a previously paused download. Registers a new cancellation token in the
/// lifecycle registry, loads chunk snapshots from the database, and spawns a resume task.
///
/// Args:
///   id:        The download row id to resume.
///   lifecycle: Tauri-managed lifecycle state.
///   app:       Tauri app handle.
///
/// Returns:
///   Ok(()) on success, Err if already active or the slot is occupied.
#[tauri::command]
pub async fn resume_download(
    id: i64,
    lifecycle: State<'_, lifecycle::LifecycleState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Reject if another download is already active.
    {
        let guard = lifecycle.0.lock().unwrap();
        if !guard.is_empty() {
            return Err("Another download is already active".to_string());
        }
    }

    let (token, intent) = lifecycle::register_download(&lifecycle, id);
    tokio::spawn(download::download_file_resume(id, app, token, intent));

    Ok(())
}
