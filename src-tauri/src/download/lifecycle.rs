//! Download lifecycle management: cancellation tokens, intent flags, and the FIFO queue.
//!
//! Each active download is represented by a `DownloadHandle` stored in `LifecycleState`.
//! `QueueState` holds the FIFO of download IDs waiting for a free slot (free tier: 1 active).

use crate::db::{self, DbState};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU8};
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
    /// Bandwidth limit in kbps (0 = unlimited). Updated live by set_download_bandwidth.
    pub bandwidth: Arc<AtomicU32>,
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
) -> (CancellationToken, Arc<AtomicU8>, Arc<AtomicU32>) {
    let token = CancellationToken::new();
    let intent = Arc::new(AtomicU8::new(intent::NONE));
    let bandwidth = Arc::new(AtomicU32::new(0));
    state.0.lock().unwrap_or_else(|p| p.into_inner()).insert(
        id,
        DownloadHandle {
            token: token.clone(),
            intent: Arc::clone(&intent),
            bandwidth: Arc::clone(&bandwidth),
        },
    );
    (token, intent, bandwidth)
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
) -> Option<(CancellationToken, Arc<AtomicU8>, Arc<AtomicU32>)> {
    let mut guard = state.0.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_empty() {
        let token = CancellationToken::new();
        let intent = Arc::new(AtomicU8::new(intent::NONE));
        let bandwidth = Arc::new(AtomicU32::new(0));
        guard.insert(
            id,
            DownloadHandle {
                token: token.clone(),
                intent: Arc::clone(&intent),
                bandwidth: Arc::clone(&bandwidth),
            },
        );
        Some((token, intent, bandwidth))
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

/// Returns true if `now_hhmm` (e.g. "14:30") falls within [start, end) (24-hour strings).
/// Handles overnight windows where end < start (e.g. "22:00" to "06:00").
///
/// Args:
///   now_hhmm: Current time as "HH:MM".
///   start:    Window start as "HH:MM".
///   end:      Window end as "HH:MM".
///
/// Returns:
///   true if now is inside the schedule window.
fn in_schedule_window(now_hhmm: &str, start: &str, end: &str) -> bool {
    fn to_mins(s: &str) -> Option<u32> {
        let mut it = s.splitn(2, ':');
        let h: u32 = it.next()?.parse().ok()?;
        let m: u32 = it.next()?.parse().ok()?;
        if h > 23 || m > 59 { return None; }
        Some(h * 60 + m)
    }
    let (Some(now), Some(s), Some(e)) = (to_mins(now_hhmm), to_mins(start), to_mins(end)) else {
        return false;
    };
    if s <= e {
        now >= s && now < e
    } else {
        // Overnight window: after start OR before end.
        now >= s || now < e
    }
}

/// Background task that runs every 60 seconds.
/// Auto-pauses active downloads outside their schedule window and auto-resumes paused
/// downloads that are inside their schedule window.
/// Spawned once at startup from `main.rs`.
///
/// Args:
///   app: Tauri app handle for accessing state and spawning resume tasks.
pub async fn schedule_watchdog(app: tauri::AppHandle) {
    use tauri::Manager;

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

        // Current local time as "HH:MM".
        let now_hhmm = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Convert UTC seconds to local HH:MM using offset.
            // Use a simple approach: rely on std (no chrono dependency).
            let secs_in_day = now % 86_400;
            // Note: this gives UTC time. For a true local-time watchdog, chrono is
            // needed. For MVP, UTC is used; users should set windows in UTC.
            let h = secs_in_day / 3600;
            let m = (secs_in_day % 3600) / 60;
            format!("{h:02}:{m:02}")
        };

        let db_state = match app.try_state::<DbState>() {
            Some(s) => s,
            None => continue,
        };

        // Pause active downloads outside their window.
        let active = {
            let conn = db_state.0.lock().unwrap_or_else(|p| p.into_inner());
            db::get_scheduled_active_downloads(&conn).unwrap_or_default()
        };
        for (id, start, end) in active {
            if !in_schedule_window(&now_hhmm, &start, &end) {
                // Fire pause intent.
                if let Some(lifecycle) = app.try_state::<LifecycleState>() {
                    let guard = lifecycle.0.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(handle) = guard.get(&id) {
                        handle.intent.store(intent::PAUSE, std::sync::atomic::Ordering::Relaxed);
                        handle.token.cancel();
                    }
                }
            }
        }

        // Resume paused downloads inside their window.
        let paused = {
            let conn = db_state.0.lock().unwrap_or_else(|p| p.into_inner());
            db::get_scheduled_paused_downloads(&conn).unwrap_or_default()
        };
        for (id, start, end) in paused {
            if in_schedule_window(&now_hhmm, &start, &end) {
                // Atomically claim the slot and register in one lock acquisition to
                // prevent a TOCTOU race where two watchdog ticks both see is_empty() true.
                if let Some(lifecycle) = app.try_state::<LifecycleState>() {
                    if let Some((token, intent_arc, bandwidth_arc)) = try_register_if_empty(&lifecycle, id) {
                        let app2 = app.clone();
                        tokio::spawn(super::download_file_resume(id, app2, token, intent_arc, bandwidth_arc));
                        break; // Only resume one download per tick.
                    }
                }
            }
        }
    }
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
    let (token, intent, bandwidth) = register_download(&lifecycle, id);

    // Restore any saved bandwidth limit so queued downloads respect previously set caps.
    if record.bandwidth_limit_kbps > 0 {
        bandwidth.store(record.bandwidth_limit_kbps as u32, std::sync::atomic::Ordering::Relaxed);
    }

    let app2 = app.clone();
    tokio::spawn(super::download_file(
        record.url,
        record.destination,
        record.filename,
        id,
        app2,
        token,
        intent,
        bandwidth,
    ));
}
