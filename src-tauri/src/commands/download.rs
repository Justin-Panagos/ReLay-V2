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

    // Reject duplicate — same URL already queued, downloading, or paused.
    {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        if db::download_exists_for_url(&conn, &url) {
            return Err("A download for this URL is already active or queued.".to_string());
        }
    }

    // Insert download row — lock, insert, unlock immediately.
    let id = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        db::insert_download(&conn, &url, &filename, &destination)
            .map_err(|e| e.to_string())?
    };

    // Atomically check slot availability and register — both under the same lock —
    // to prevent a TOCTOU race where two concurrent commands both see an empty
    // registry and each spawn a download, violating the free-tier 1-active limit.
    let lifecycle = app.state::<lifecycle::LifecycleState>();
    if let Some((token, intent)) = lifecycle::try_register_if_empty(&lifecycle, id) {
        let app2 = app.clone();
        tokio::spawn(download::download_file(
            url, destination, filename, id, app2, token, intent,
        ));
    } else {
        let queue = app.state::<lifecycle::QueueState>();
        queue.0.lock().unwrap_or_else(|p| p.into_inner()).push_back(id);
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
    let guard = lifecycle.0.lock().unwrap_or_else(|p| p.into_inner());
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
        let guard = lifecycle.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = guard.get(&id) {
            handle.intent.store(lifecycle::intent::CANCEL, Ordering::Relaxed);
            handle.token.cancel();
            return Ok(());
        }
    }

    // Case 2: in the FIFO queue — remove it so it never starts.
    {
        let mut q = queue.0.lock().unwrap_or_else(|p| p.into_inner());
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
        let guard = lifecycle.0.lock().unwrap_or_else(|p| p.into_inner());
        if !guard.is_empty() {
            return Err("Another download is already active".to_string());
        }
    }

    let (token, intent) = lifecycle::register_download(&lifecycle, id);
    tokio::spawn(download::download_file_resume(id, app, token, intent));

    Ok(())
}

/// Opens a native folder picker dialog and returns the selected path.
/// Returns None if the user dismisses the dialog without selecting a folder.
///
/// Uses the async (callback-based) dialog API with a oneshot channel instead of
/// the blocking variant. The blocking API dispatches to the main thread via an
/// internal channel which can deadlock on macOS when called from a Tauri command
/// handler thread — the async approach avoids that entirely.
///
/// Returns:
///   Some(path) with the selected directory as a string, or None if cancelled.
#[tauri::command]
pub async fn pick_folder() -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tauri::api::dialog::FileDialogBuilder::new().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| p.to_string_lossy().to_string()));
    });
    rx.await.ok().flatten()
}

/// Appends a timestamped error entry to relay_errors.log in the app data directory.
/// Called from the frontend whenever a user-triggered action fails, so errors are
/// available for debugging even when the DevTools console is not open.
///
/// Args:
///   app_handle: Tauri app handle used to resolve the app data directory.
///   action:     Short description of the action that failed (e.g. "resume_download").
///   message:    The error message string.
///
/// Returns:
///   Ok(()) on success, Err if the log file cannot be written.
#[tauri::command]
pub fn log_error(
    app_handle: tauri::AppHandle,
    action: String,
    message: String,
) -> Result<(), String> {
    let dir = app_handle
        .path_resolver()
        .app_data_dir()
        .ok_or("no app data dir")?;
    let path = dir.join("relay_errors.log");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let line = format!("[{ts}] {action} \u{2014} {message}\n");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()))
        .map_err(|e| e.to_string())
}
