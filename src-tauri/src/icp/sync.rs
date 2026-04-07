use std::time::Duration;

use tauri::Manager;

use crate::db::{self, DbState};
use crate::icp::agent::{self, IcpError, LicenceStatus};
use crate::icp::AppConfig;

/// Runs once at app startup:
///   1. Ensures a UUID v4 device_id exists in settings (generates one if absent).
///   2. Calls identity::register_device (idempotent on canister).
///   3. Calls identity::check_licence → writes "pro" or "free" to licence_status setting.
///   4. Runs the first pattern sync immediately.
///   5. Spawns the 12-hour background pattern sync loop.
///
/// All ICP failures are logged to stderr but non-fatal — the app must start even
/// if the ICP network is unreachable.
///
/// Args:
///   app:    Tauri app handle for accessing managed state.
///   config: Loaded AppConfig with canister IDs and icp_url.
pub async fn run_startup(app: tauri::AppHandle, config: AppConfig) {
    // ── Step 1: ensure device_id ────────────────────────────────────────────
    let device_id = {
        let db_state = app.state::<DbState>();
        let conn = db_state.0.lock().unwrap();
        match db::get_setting(&conn, "device_id")
            .ok()
            .flatten()
        {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                db::set_setting(&conn, "device_id", &id).ok();
                id
            }
        }
    };

    // Build agent — if this fails the ICP network is unreachable; log and skip all ICP calls.
    let agent = match agent::build_agent(&config.icp_url).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[icp] could not build agent: {e}");
            return;
        }
    };

    let identity_id = match agent::parse_principal(&config.canisters.identity) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[icp] invalid identity canister id: {e}");
            return;
        }
    };

    // ── Step 2: register device (idempotent) ────────────────────────────────
    if let Err(e) = agent::register_device(&agent, &identity_id, device_id.clone()).await {
        eprintln!("[icp] register_device failed: {e}");
    }

    // ── Step 3: sync licence status ─────────────────────────────────────────
    match agent::check_licence(&agent, &identity_id, device_id.clone()).await {
        Ok(LicenceStatus::Pro { expiry_timestamp }) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let status = if expiry_timestamp > now { "pro" } else { "free" };
            let db_state = app.state::<DbState>();
            let conn = db_state.0.lock().unwrap();
            db::set_setting(&conn, "licence_status", status).ok();
            db::set_setting(&conn, "licence_expiry", &expiry_timestamp.to_string()).ok();
        }
        Ok(LicenceStatus::Free) => {
            let db_state = app.state::<DbState>();
            let conn = db_state.0.lock().unwrap();
            db::set_setting(&conn, "licence_status", "free").ok();
        }
        Err(e) => {
            eprintln!("[icp] check_licence failed: {e}");
            // Do not overwrite existing licence_status — fall through silently.
        }
    }

    // ── Step 4: first pattern sync ──────────────────────────────────────────
    if let Err(e) = sync_patterns_once(&app, &config).await {
        eprintln!("[icp] initial pattern sync failed: {e}");
    }

    // ── Step 5: spawn 12-hour loop ──────────────────────────────────────────
    spawn_pattern_sync_loop(app, config);
}

/// Spawns a Tokio task that runs a pattern sync immediately, then repeats every 12 hours.
/// MissedTickBehavior::Skip prevents back-to-back ticks if a sync takes longer than 12 hours.
///
/// Args:
///   app:    Tauri app handle.
///   config: Loaded AppConfig.
pub fn spawn_pattern_sync_loop(app: tauri::AppHandle, config: AppConfig) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(12 * 60 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(e) = sync_patterns_once(&app, &config).await {
                eprintln!("[icp] pattern sync failed: {e}");
            }
        }
    });
}

/// Fetches new pattern entries since the last known id, upserts them into the
/// local `icp_patterns` SQLite table, and updates the `last_pattern_id` setting.
///
/// Args:
///   app:    Tauri app handle.
///   config: Loaded AppConfig.
///
/// Returns:
///   Ok(()) on success, Err(IcpError) on network or DB failure.
async fn sync_patterns_once(app: &tauri::AppHandle, config: &AppConfig) -> Result<(), IcpError> {
    let db_state = app.state::<DbState>();

    // Read last_pattern_id — lock, read, release immediately.
    let since_id: u64 = {
        let conn = db_state.0.lock().unwrap();
        db::get_setting(&conn, "last_pattern_id")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };

    let agent = agent::build_agent(&config.icp_url).await?;
    let pattern_id =
        agent::parse_principal(&config.canisters.pattern)?;

    let entries = agent::get_delta_since(&agent, &pattern_id, since_id).await?;

    if entries.is_empty() {
        return Ok(());
    }

    let new_last_id = entries.last().map(|e| e.id).unwrap_or(since_id);

    {
        let conn = db_state.0.lock().unwrap();
        db::upsert_icp_patterns(&conn, &entries)
            .map_err(|e| IcpError::Decode(e.to_string()))?;
        db::set_setting(&conn, "last_pattern_id", &new_last_id.to_string())
            .map_err(|e| IcpError::Decode(e.to_string()))?;
    }

    eprintln!(
        "[icp] synced {} new patterns (last_id={})",
        entries.len(),
        new_last_id
    );
    Ok(())
}
