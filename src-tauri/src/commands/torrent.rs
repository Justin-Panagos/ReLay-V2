use crate::db::{self, DbState};
use crate::torrent::{
    self, TorrentErrorPayload, TorrentFileEntry, TorrentPollerState, TorrentSessionState,
};
use tauri::{Manager, State};
use tokio_util::sync::CancellationToken;

/// Adds a magnet link as a new torrent download.
/// Inserts a DB row, calls the librqbit session, stores the returned torrent id,
/// registers a progress poller cancellation token, and returns the DB row id.
///
/// Args:
///   magnet:   The magnet URI string.
///   destination: Optional save directory. Defaults to the `default_folder` setting.
///   app:      Tauri app handle for event emission and DB access.
///   db:       Tauri-managed database state.
///   session:  Tauri-managed librqbit session state.
///   pollers:  Tauri-managed poller cancellation token map.
///
/// Returns:
///   Ok(db_id) — the downloads table row id.
///   Err(message) on any failure.
#[tauri::command]
pub async fn add_magnet(
    magnet: String,
    destination: Option<String>,
    app: tauri::AppHandle,
    db: State<'_, DbState>,
    session: State<'_, TorrentSessionState>,
    pollers: State<'_, TorrentPollerState>,
) -> Result<i64, String> {
    let dest = resolve_destination(&db, destination)?;

    // Reject duplicate magnet already active or queued.
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        if db::torrent_exists_for_url(&conn, &magnet) {
            return Err("A torrent with this magnet link is already active or queued.".to_string());
        }
    }

    let db_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::insert_torrent(&conn, &magnet, "Resolving...", &dest)
            .map_err(|e| e.to_string())?
    };

    let cancel_token = CancellationToken::new();
    pollers
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(db_id, cancel_token.clone());

    // Extract the Arc<Session> — State<'_, T> has a tied lifetime and cannot
    // be moved into a 'static spawn closure directly.
    let session_arc = session.0.clone();
    let app2 = app.clone();

    // Spawn the session work in the background and return db_id immediately.
    // For magnet links session.add_torrent() does DHT metadata resolution which
    // can take 10–60 s — awaiting it directly would freeze the UI.
    tokio::spawn(async move {
        match torrent::add_magnet(
            &session_arc,
            magnet,
            dest,
            None,
            app2.clone(),
            db_id,
            cancel_token,
        )
        .await
        {
            Ok(torrent_id) => {
                if let Some(db_state) = app2.try_state::<DbState>() {
                    let conn = db_state.0.lock().unwrap_or_else(|p| p.into_inner());
                    let _ = db::update_torrent_id(&conn, db_id, torrent_id);
                    let _ = db::update_download_status(&conn, db_id, "downloading");
                }
            }
            Err(e) => {
                if let Some(db_state) = app2.try_state::<DbState>() {
                    let conn = db_state.0.lock().unwrap_or_else(|p| p.into_inner());
                    let _ = db::update_download_status(&conn, db_id, "failed");
                }
                let _ = app2.emit_all(
                    &format!("torrent://error/{db_id}"),
                    TorrentErrorPayload { message: e },
                );
            }
        }
    });

    Ok(db_id)
}

/// Lists the files contained in a .torrent file without starting the download.
/// Used to populate the file-selection modal.
///
/// Args:
///   file_bytes: Raw bytes of the .torrent file as a Vec<u8>.
///   session:    Tauri-managed librqbit session state.
///
/// Returns:
///   Ok(Vec<TorrentFileEntry>) describing every file in the torrent.
///   Err(message) on parse or session failure.
#[tauri::command]
pub async fn list_torrent_files(
    file_bytes: Vec<u8>,
    session: State<'_, TorrentSessionState>,
) -> Result<Vec<TorrentFileEntry>, String> {
    torrent::list_torrent_files(&session.0, file_bytes).await
}

/// Adds a .torrent file as a new torrent download with an optional file selection.
/// Inserts a DB row, calls the librqbit session, stores the torrent id, registers
/// a progress poller cancellation token, and returns the DB row id.
///
/// Args:
///   file_bytes:  Raw bytes of the .torrent file.
///   destination: Directory path where files should be saved.
///   only_files:  Indices of files to download (empty Vec = download all).
///   app:         Tauri app handle.
///   db:          Tauri-managed database state.
///   session:     Tauri-managed librqbit session state.
///   pollers:     Tauri-managed poller cancellation token map.
///
/// Returns:
///   Ok(db_id) on success.
///   Err(message) on any failure.
#[tauri::command]
pub async fn add_torrent_file(
    file_bytes: Vec<u8>,
    destination: String,
    only_files: Vec<usize>,
    app: tauri::AppHandle,
    db: State<'_, DbState>,
    session: State<'_, TorrentSessionState>,
    pollers: State<'_, TorrentPollerState>,
) -> Result<i64, String> {
    let selected = if only_files.is_empty() {
        None
    } else {
        Some(only_files)
    };

    let db_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::insert_torrent(&conn, "torrent-file", "Loading...", &destination)
            .map_err(|e| e.to_string())?
    };

    let cancel_token = CancellationToken::new();
    pollers
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(db_id, cancel_token.clone());

    let session_arc = session.0.clone();
    let app2 = app.clone();

    // Spawn in the background for the same reason as add_magnet — consistent
    // fire-and-forget pattern. For .torrent bytes this resolves quickly, but
    // keeping the pattern avoids any blocking on the command thread.
    tokio::spawn(async move {
        match torrent::add_torrent_file(
            &session_arc,
            file_bytes,
            destination,
            selected,
            app2.clone(),
            db_id,
            cancel_token,
        )
        .await
        {
            Ok(torrent_id) => {
                if let Some(db_state) = app2.try_state::<DbState>() {
                    let conn = db_state.0.lock().unwrap_or_else(|p| p.into_inner());
                    let _ = db::update_torrent_id(&conn, db_id, torrent_id);
                    let _ = db::update_download_status(&conn, db_id, "downloading");
                }
            }
            Err(e) => {
                if let Some(db_state) = app2.try_state::<DbState>() {
                    let conn = db_state.0.lock().unwrap_or_else(|p| p.into_inner());
                    let _ = db::update_download_status(&conn, db_id, "failed");
                }
                let _ = app2.emit_all(
                    &format!("torrent://error/{db_id}"),
                    TorrentErrorPayload { message: e },
                );
            }
        }
    });

    Ok(db_id)
}

/// Pauses a torrent that is currently active or initializing.
/// Looks up the librqbit torrent_id from the DB, calls the session pause, and
/// updates the DB status to `paused`.
///
/// Args:
///   id:      The DB downloads row id.
///   db:      Tauri-managed database state.
///   session: Tauri-managed librqbit session state.
///
/// Returns:
///   Ok(()) on success, Err with message on failure.
#[tauri::command]
pub async fn pause_torrent(
    id: i64,
    db: State<'_, DbState>,
    session: State<'_, TorrentSessionState>,
) -> Result<(), String> {
    eprintln!("[torrent] pause_torrent id={id}");
    let torrent_id = get_torrent_id(&db, id)?;
    torrent::pause_torrent(&session.0, torrent_id as usize).await?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_download_status(&conn, id, "paused").map_err(|e| e.to_string())
}

/// Resumes a paused torrent via the librqbit session and updates the DB status.
///
/// Args:
///   id:      The DB downloads row id.
///   db:      Tauri-managed database state.
///   session: Tauri-managed librqbit session state.
///
/// Returns:
///   Ok(()) on success, Err with message on failure.
#[tauri::command]
pub async fn resume_torrent(
    id: i64,
    db: State<'_, DbState>,
    session: State<'_, TorrentSessionState>,
) -> Result<(), String> {
    eprintln!("[torrent] resume_torrent id={id}");
    let torrent_id = get_torrent_id(&db, id)?;
    torrent::resume_torrent(&session.0, torrent_id as usize).await?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_download_status(&conn, id, "downloading").map_err(|e| e.to_string())
}

/// Cancels a torrent by cancelling the progress poller and removing it from
/// the librqbit session. Does not delete downloaded files. Updates DB to `failed`.
///
/// Args:
///   id:      The DB downloads row id.
///   db:      Tauri-managed database state.
///   session: Tauri-managed librqbit session state.
///   pollers: Tauri-managed poller cancellation token map.
///
/// Returns:
///   Ok(()) always — cancel is best-effort.
#[tauri::command]
pub async fn cancel_torrent(
    id: i64,
    db: State<'_, DbState>,
    session: State<'_, TorrentSessionState>,
    pollers: State<'_, TorrentPollerState>,
) -> Result<(), String> {
    eprintln!("[torrent] cancel_torrent id={id}");
    // Cancel the progress poller first.
    if let Some(token) = pollers.0.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) {
        token.cancel();
    }

    // Remove from session if a torrent_id is stored.
    if let Ok(torrent_id) = get_torrent_id(&db, id) {
        torrent::cancel_torrent(&session.0, torrent_id as usize, false)
            .await
            .ok();
    }

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_download_status(&conn, id, "failed").map_err(|e| e.to_string())
}

/// Returns recent torrent rows from the database (type = 'torrent'), newest first.
///
/// Args:
///   limit: Maximum number of rows to return.
///   db:    Tauri-managed database state.
///
/// Returns:
///   Vec of DownloadRecord on success, or an error string.
#[tauri::command]
pub fn get_torrents(
    limit: i64,
    db: State<'_, DbState>,
) -> Result<Vec<db::DownloadRecord>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::get_torrents(&conn, limit).map_err(|e| e.to_string())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Reads the `default_folder` setting from the DB and returns it as a String,
/// or falls back to "." if the setting is not set. Used when the caller passes
/// `destination: None`.
///
/// Args:
///   db:          Tauri-managed database state.
///   destination: Optional caller-supplied destination override.
///
/// Returns:
///   The resolved destination directory path string.
fn resolve_destination(db: &State<'_, DbState>, destination: Option<String>) -> Result<String, String> {
    if let Some(d) = destination {
        return Ok(d);
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    Ok(db::get_setting(&conn, "default_folder")
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| ".".to_string()))
}

/// Looks up the librqbit torrent_id stored in the DB for a given download row.
///
/// Args:
///   db: Tauri-managed database state.
///   id: The DB downloads row id.
///
/// Returns:
///   The torrent_id (i64) if found, or an error string.
fn get_torrent_id(db: &State<'_, DbState>, id: i64) -> Result<i64, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    match db::get_download_by_id(&conn, id).map_err(|e| e.to_string())? {
        Some(record) => record
            .torrent_id
            .ok_or_else(|| format!("torrent_id not yet assigned for download {id}")),
        None => Err(format!("no download row with id {id}")),
    }
}
