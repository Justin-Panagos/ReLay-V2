use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session,
    TorrentStatsState,
};
use librqbit::api::TorrentIdOrHash;
use serde::Serialize;
use tauri::Manager;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;

use crate::db::{self, DbState};

// ── State types ─────────────────────────────────────────────────────────────

/// Tauri-managed state holding the single librqbit Session for all torrents.
pub struct TorrentSessionState(pub Arc<Session>);

/// Tauri-managed state holding a per-download cancellation token for each
/// active torrent progress poller, keyed by the DB download id.
pub struct TorrentPollerState(pub Mutex<HashMap<i64, CancellationToken>>);

// ── Payload types ────────────────────────────────────────────────────────────

/// Progress event payload emitted as `torrent://progress/{db_id}`.
#[derive(Clone, Serialize)]
pub struct TorrentProgressPayload {
    /// Bytes downloaded and verified so far.
    pub downloaded: u64,
    /// Total bytes in the torrent (0 if not yet known).
    pub total: u64,
    /// Number of currently live (connected) peers.
    pub peers: usize,
    /// Upload ratio: uploaded / downloaded. 0.0 when downloaded == 0.
    pub ratio: f64,
    /// Number of pieces downloaded and checked.
    pub pieces: u64,
    /// Current torrent state string: "initializing" | "live" | "paused" | "error".
    pub state: String,
    /// Download speed in bytes per second.
    pub speed_bps: u64,
}

/// Payload for the `torrent://complete/{db_id}` event.
#[derive(Clone, Serialize)]
pub struct TorrentCompletePayload {
    /// Destination folder path.
    pub path: String,
}

/// Payload for the `torrent://error/{db_id}` event.
#[derive(Clone, Serialize)]
pub struct TorrentErrorPayload {
    /// Human-readable error message.
    pub message: String,
}

/// A single file entry returned by `list_torrent_files`, used to populate the
/// .torrent selection modal.
#[derive(Serialize)]
pub struct TorrentFileEntry {
    /// Zero-based index of the file within the torrent.
    pub index: usize,
    /// File name (last path component, or full relative path for multi-file).
    pub name: String,
    /// File size in bytes.
    pub size_bytes: u64,
}

// ── Core functions ───────────────────────────────────────────────────────────

/// Adds a magnet link to the librqbit session and spawns a progress poller.
///
/// Args:
///   session:    Arc reference to the active librqbit Session.
///   magnet:     The magnet URI string.
///   destination: Directory path where files should be saved.
///   only_files: Optional list of file indices to download (None = all files).
///   app:        Tauri AppHandle used for event emission and DB access.
///   db_id:      The DB downloads row id for this torrent.
///   cancel_token: CancellationToken to stop the progress poller on cancel.
///
/// Returns:
///   The librqbit TorrentId (usize) assigned by the session.
pub async fn add_magnet(
    session: &Arc<Session>,
    magnet: String,
    destination: String,
    only_files: Option<Vec<usize>>,
    app: tauri::AppHandle,
    db_id: i64,
    cancel_token: CancellationToken,
) -> Result<usize, String> {
    let opts = AddTorrentOptions {
        output_folder: Some(destination.clone()),
        only_files,
        overwrite: true,
        ..Default::default()
    };

    let response = session
        .add_torrent(AddTorrent::from_url(magnet), Some(opts))
        .await
        .map_err(|e| e.to_string())?;

    handle_add_response(response, destination, app, db_id, cancel_token, session.clone())
}

/// Adds a .torrent file from raw bytes to the librqbit session and spawns a
/// progress poller.
///
/// Args:
///   session:    Arc reference to the active librqbit Session.
///   file_bytes: Raw bytes of the .torrent file.
///   destination: Directory path where files should be saved.
///   only_files: Optional list of file indices to download (None = all files).
///   app:        Tauri AppHandle used for event emission and DB access.
///   db_id:      The DB downloads row id for this torrent.
///   cancel_token: CancellationToken to stop the progress poller on cancel.
///
/// Returns:
///   The librqbit TorrentId (usize) assigned by the session.
pub async fn add_torrent_file(
    session: &Arc<Session>,
    file_bytes: Vec<u8>,
    destination: String,
    only_files: Option<Vec<usize>>,
    app: tauri::AppHandle,
    db_id: i64,
    cancel_token: CancellationToken,
) -> Result<usize, String> {
    let opts = AddTorrentOptions {
        output_folder: Some(destination.clone()),
        only_files,
        overwrite: true,
        ..Default::default()
    };

    let response = session
        .add_torrent(AddTorrent::from_bytes(file_bytes), Some(opts))
        .await
        .map_err(|e| e.to_string())?;

    handle_add_response(response, destination, app, db_id, cancel_token, session.clone())
}

/// Lists the files in a .torrent file without starting the download.
/// Used to populate the file-selection modal before the user confirms.
///
/// Args:
///   session:      Arc reference to the active librqbit Session.
///   torrent_bytes: Raw bytes of the .torrent file.
///
/// Returns:
///   Vec of TorrentFileEntry structs describing each file in the torrent.
pub async fn list_torrent_files(
    session: &Arc<Session>,
    torrent_bytes: Vec<u8>,
) -> Result<Vec<TorrentFileEntry>, String> {
    let opts = AddTorrentOptions {
        list_only: true,
        ..Default::default()
    };

    let response = session
        .add_torrent(AddTorrent::from_bytes(torrent_bytes), Some(opts))
        .await
        .map_err(|e| e.to_string())?;

    match response {
        AddTorrentResponse::ListOnly(resp) => {
            let entries = resp
                .info
                .iter_file_details()
                .map_err(|e| e.to_string())?
                .enumerate()
                .map(|(index, detail)| TorrentFileEntry {
                    index,
                    name: detail
                        .filename
                        .to_string()
                        .unwrap_or_else(|_| format!("file_{index}")),
                    size_bytes: detail.len,
                })
                .collect();
            Ok(entries)
        }
        _ => Err("unexpected response when listing torrent files".into()),
    }
}

/// Pauses an active torrent via the librqbit session.
///
/// Args:
///   session:    Arc reference to the active librqbit Session.
///   torrent_id: The librqbit TorrentId (stored as usize in DB).
///
/// Returns:
///   Ok(()) on success, Err with message on failure.
pub async fn pause_torrent(session: &Arc<Session>, torrent_id: usize) -> Result<(), String> {
    let handle = session
        .get(TorrentIdOrHash::Id(torrent_id))
        .ok_or_else(|| format!("torrent {torrent_id} not found in session"))?;
    session
        .pause(&handle)
        .await
        .map_err(|e| e.to_string())
}

/// Resumes a paused torrent via the librqbit session.
///
/// Args:
///   session:    Arc reference to the active librqbit Session.
///   torrent_id: The librqbit TorrentId (stored as usize in DB).
///
/// Returns:
///   Ok(()) on success, Err with message on failure.
pub async fn resume_torrent(session: &Arc<Session>, torrent_id: usize) -> Result<(), String> {
    let handle = session
        .get(TorrentIdOrHash::Id(torrent_id))
        .ok_or_else(|| format!("torrent {torrent_id} not found in session"))?;
    session
        .unpause(&handle)
        .await
        .map_err(|e| e.to_string())
}

/// Removes a torrent from the librqbit session, optionally deleting downloaded files.
///
/// Args:
///   session:      Arc reference to the active librqbit Session.
///   torrent_id:   The librqbit TorrentId.
///   delete_files: Whether to delete partially or fully downloaded files.
///
/// Returns:
///   Ok(()) on success, Err with message on failure.
pub async fn cancel_torrent(
    session: &Arc<Session>,
    torrent_id: usize,
    delete_files: bool,
) -> Result<(), String> {
    session
        .delete(TorrentIdOrHash::Id(torrent_id), delete_files)
        .await
        .map_err(|e| e.to_string())
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Processes an AddTorrentResponse after calling session.add_torrent().
/// Spawns the progress poller for Added/AlreadyManaged responses.
///
/// Args:
///   response:     The AddTorrentResponse from the session.
///   destination:  Destination directory path (for complete event payload).
///   app:          Tauri AppHandle.
///   db_id:        DB downloads row id.
///   cancel_token: CancellationToken for the progress poller.
///   session:      Arc to the session (passed to poller for cleanup).
///
/// Returns:
///   The librqbit TorrentId on success.
fn handle_add_response(
    response: AddTorrentResponse,
    destination: String,
    app: tauri::AppHandle,
    db_id: i64,
    cancel_token: CancellationToken,
    _session: Arc<Session>,
) -> Result<usize, String> {
    match response {
        AddTorrentResponse::Added(tid, handle) => {
            spawn_progress_poller(handle, db_id, destination, app, cancel_token);
            Ok(tid)
        }
        AddTorrentResponse::AlreadyManaged(tid, handle) => {
            spawn_progress_poller(handle, db_id, destination, app, cancel_token);
            Ok(tid)
        }
        AddTorrentResponse::ListOnly(_) => {
            Err("add_torrent returned ListOnly — list_only flag was not set".into())
        }
    }
}

/// Spawns a background tokio task that polls torrent stats every 500 ms and
/// emits Tauri events. Exits when the torrent finishes, errors, or the
/// cancellation token fires.
///
/// Args:
///   handle:       librqbit Arc<ManagedTorrent> to poll stats from.
///   db_id:        DB downloads row id (used in event names and DB updates).
///   destination:  Destination path sent in the complete event payload.
///   app:          Tauri AppHandle for event emission and DB access.
///   cancel_token: Token cancelled by the cancel command.
pub fn spawn_progress_poller(
    handle: Arc<ManagedTorrent>,
    db_id: i64,
    destination: String,
    app: tauri::AppHandle,
    cancel_token: CancellationToken,
) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }
                _ = ticker.tick() => {
                    let stats = handle.stats();

                    // Update DB with latest progress.
                    if let Some(db_state) = app.try_state::<DbState>() {
                        let conn = db_state.0.lock().unwrap();
                        let _ = db::update_downloaded_bytes(
                            &conn,
                            db_id,
                            stats.progress_bytes,
                            stats.total_bytes,
                        );
                    }

                    // Build peer count and ratio.
                    let (peers, ratio, speed_bps) = match &stats.live {
                        Some(live) => {
                            let peers = live.snapshot.peer_stats.live;
                            let ratio = if stats.progress_bytes > 0 {
                                stats.uploaded_bytes as f64 / stats.progress_bytes as f64
                            } else {
                                0.0
                            };
                            let speed_bps =
                                (live.download_speed.mbps * 1_048_576.0) as u64;
                            (peers, ratio, speed_bps)
                        }
                        None => (0, 0.0, 0),
                    };

                    let state_str = match stats.state {
                        TorrentStatsState::Initializing => "initializing",
                        TorrentStatsState::Live => "live",
                        TorrentStatsState::Paused => "paused",
                        TorrentStatsState::Error => "error",
                    };

                    let pieces = stats
                        .live
                        .as_ref()
                        .map(|l| l.snapshot.downloaded_and_checked_pieces)
                        .unwrap_or(0);

                    let payload = TorrentProgressPayload {
                        downloaded: stats.progress_bytes,
                        total: stats.total_bytes,
                        peers,
                        ratio,
                        pieces,
                        state: state_str.to_string(),
                        speed_bps,
                    };

                    let _ = app.emit_all(
                        &format!("torrent://progress/{db_id}"),
                        payload,
                    );

                    // Check terminal states.
                    if stats.finished {
                        if let Some(db_state) = app.try_state::<DbState>() {
                            let conn = db_state.0.lock().unwrap();
                            let _ = db::update_download_status(&conn, db_id, "complete");
                        }
                        let _ = app.emit_all(
                            &format!("torrent://complete/{db_id}"),
                            TorrentCompletePayload { path: destination },
                        );
                        break;
                    }

                    if matches!(stats.state, TorrentStatsState::Error) {
                        let msg = stats
                            .error
                            .unwrap_or_else(|| "unknown torrent error".to_string());
                        if let Some(db_state) = app.try_state::<DbState>() {
                            let conn = db_state.0.lock().unwrap();
                            let _ = db::update_download_status(&conn, db_id, "failed");
                        }
                        let _ = app.emit_all(
                            &format!("torrent://error/{db_id}"),
                            TorrentErrorPayload { message: msg },
                        );
                        break;
                    }
                }
            }
        }
    });
}
