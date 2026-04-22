//! Download lifecycle management: cancellation tokens, intent flags, and the FIFO queue.
//!
//! Each active download is represented by a `DownloadHandle` stored in `LifecycleState`.
//! `QueueState` holds the FIFO of download IDs waiting for a free slot (free tier: 1 active).

use crate::db::{self, DbState};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Discriminates why a `CancellationToken` was fired.
/// Written by `pause_download` / `cancel_download` before calling `token.cancel()`.
pub mod intent {
    pub const NONE: u8 = 0;
    pub const PAUSE: u8 = 1;
    pub const CANCEL: u8 = 2;
}

/// Live handle for an in-flight download.
pub struct DownloadHandle {
    /// Fires to interrupt the download task.
    pub token: CancellationToken,
    /// Set to `intent::PAUSE` or `intent::CANCEL` before calling `token.cancel()`.
    pub intent: Arc<AtomicU8>,
}

/// Tauri-managed state: one `DownloadHandle` per active download, keyed by download id.
pub struct LifecycleState(pub Mutex<HashMap<i64, DownloadHandle>>);

/// Tauri-managed state: FIFO queue of download ids waiting for a free slot.
pub struct QueueState(pub Mutex<VecDeque<i64>>);

/// Creates and registers a new `DownloadHandle` for `id`, returning clones of the token
/// and intent for passing into the download task.
///
/// Args:
///   state: Reference to the LifecycleState managed by Tauri.
///   id:    The download row id to register.
///
/// Returns:
///   (CancellationToken, Arc<AtomicU8>) — pass both into `download_file`.
pub fn register_download(
    state: &LifecycleState,
    id: i64,
) -> (CancellationToken, Arc<AtomicU8>) {
    let token = CancellationToken::new();
    let intent = Arc::new(AtomicU8::new(intent::NONE));
    state.0.lock().unwrap_or_else(|p| p.into_inner()).insert(
        id,
        DownloadHandle {
            token: token.clone(),
            intent: Arc::clone(&intent),
        },
    );
    (token, intent)
}

/// Atomically checks whether the registry is empty and, if so, registers the download.
/// Returns `Some((token, intent))` if a slot was claimed, or `None` if occupied.
/// This is the safe way to implement a 1-active limit without a TOCTOU race — callers
/// must not split the emptiness check and the registration across two separate lock
/// acquisitions.
///
/// Args:
///   state: Reference to the LifecycleState managed by Tauri.
///   id:    The download row id to register if a slot is free.
///
/// Returns:
///   Some((CancellationToken, Arc<AtomicU8>)) if the slot was claimed, None otherwise.
pub fn try_register_if_empty(
    state: &LifecycleState,
    id: i64,
) -> Option<(CancellationToken, Arc<AtomicU8>)> {
    let mut guard = state.0.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_empty() {
        let token = CancellationToken::new();
        let intent = Arc::new(AtomicU8::new(intent::NONE));
        guard.insert(
            id,
            DownloadHandle {
                token: token.clone(),
                intent: Arc::clone(&intent),
            },
        );
        Some((token, intent))
    } else {
        None
    }
}

/// Removes and returns the `DownloadHandle` for `id`.
/// Called at the end of every `download_file` execution (all outcomes).
///
/// Args:
///   state: Reference to the LifecycleState managed by Tauri.
///   id:    The download row id to deregister.
///
/// Returns:
///   The removed handle, or None if id was not registered.
pub fn deregister_download(state: &LifecycleState, id: i64) -> Option<DownloadHandle> {
    state.0.lock().unwrap_or_else(|p| p.into_inner()).remove(&id)
}

/// Pops the next queued download id from `QueueState` and spawns its download task.
/// Called after every `download_file` outcome so the queue drains automatically.
/// This is a plain (non-async) function — all operations are synchronous lock/unlock;
/// the new download task is launched via `tokio::spawn` and runs independently.
///
/// Args:
///   app: Tauri app handle — used to access managed state and spawn the new task.
pub fn try_start_next(app: tauri::AppHandle) {
    use tauri::Manager;

    let next_id = {
        let queue = app.state::<QueueState>();
        let id = queue.0.lock().unwrap_or_else(|p| p.into_inner()).pop_front();
        id
    };

    let Some(id) = next_id else { return };

    // Load the download record so we have url / destination / filename.
    let record = {
        let db = app.state::<DbState>();
        let conn = db.0.lock().unwrap_or_else(|p| p.into_inner());
        db::get_download_by_id(&conn, id).ok().flatten()
    };

    let Some(record) = record else {
        // Row missing — silently skip.
        return;
    };

    let lifecycle = app.state::<LifecycleState>();
    let (token, intent) = register_download(&lifecycle, id);

    let app2 = app.clone();
    tokio::spawn(super::download_file(
        record.url,
        record.destination,
        record.filename,
        id,
        app2,
        token,
        intent,
    ));
}
