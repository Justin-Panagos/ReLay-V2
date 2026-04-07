use crate::db::{self, DbState};
use crate::icp::{agent, ConfigState};
use tauri::{Manager, State};

/// Licence info returned to the frontend for display in Settings → Pro.
#[derive(serde::Serialize)]
pub struct ProStatus {
    /// "pro" or "free".
    pub status: String,
    /// Unix seconds when the Pro licence expires. None if Free or expiry is 0.
    pub expiry_timestamp: Option<u64>,
    /// The device's UUID registered with the ICP Identity Canister.
    pub device_id: String,
}

/// Returns the current licence status, expiry, and device ID from local settings.
/// Reads from the SQLite settings table — does not make a network call.
///
/// Args:
///   db: Tauri-managed database state.
///
/// Returns:
///   ProStatus with values read from the settings table.
#[tauri::command]
pub fn get_pro_status(db: State<'_, DbState>) -> Result<ProStatus, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let status = db::get_setting(&conn, "licence_status")
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| "free".to_string());
    let expiry_timestamp = db::get_setting(&conn, "licence_expiry")
        .map_err(|e| e.to_string())?
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&ts| ts > 0);
    let device_id = db::get_setting(&conn, "device_id")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(ProStatus {
        status,
        expiry_timestamp,
        device_id,
    })
}

/// Initialises a Paystack checkout session via the Cloudflare Worker and opens
/// the resulting authorization URL in the system browser.
/// The Worker embeds device_id in transaction metadata so the webhook can map
/// the completed payment back to this device.
///
/// Args:
///   db:     Tauri-managed database state (reads device_id).
///   config: Tauri-managed ICP config state (reads worker_url).
///   client: Shared reqwest HTTP client.
///   app:    Tauri app handle (used for shell::open).
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
#[tauri::command]
pub async fn open_upgrade_page(
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let device_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "device_id not set — app not fully initialised".to_string())?
    };

    let worker_url = config
        .0
        .worker_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "worker_url not set in config.toml — add it and restart".to_string())?
        .to_string();

    let resp = client
        .post(format!("{worker_url}/init-checkout"))
        .json(&serde_json::json!({ "device_id": device_id }))
        .send()
        .await
        .map_err(|e| format!("could not reach payment server: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("payment server returned {}", resp.status()));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let authorization_url = json["authorization_url"]
        .as_str()
        .ok_or_else(|| "payment server returned no authorization_url".to_string())?
        .to_string();

    tauri::api::shell::open(&app.shell_scope(), authorization_url, None)
        .map_err(|e| e.to_string())
}

/// Re-checks the licence status against the ICP Identity Canister and updates
/// `licence_status` and `licence_expiry` in the local settings table.
/// Used by the "Re-check" button in Settings → Pro after a user upgrades.
///
/// Args:
///   db:     Tauri-managed database state.
///   config: Tauri-managed ICP config state.
///
/// Returns:
///   Ok(()) on success. ICP network errors are returned as Err strings.
#[tauri::command]
pub async fn recheck_licence(
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
) -> Result<(), String> {
    let device_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "device_id not set".to_string())?
    };

    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url)
        .await
        .map_err(|e| e.to_string())?;
    let identity_id =
        agent::parse_principal(&icp.canisters.identity).map_err(|e| e.to_string())?;

    let (status, expiry) =
        match agent::check_licence(&a, &identity_id, device_id)
            .await
            .map_err(|e| e.to_string())?
        {
            agent::LicenceStatus::Pro { expiry_timestamp } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if expiry_timestamp > now {
                    ("pro".to_string(), expiry_timestamp.to_string())
                } else {
                    ("free".to_string(), "0".to_string())
                }
            }
            agent::LicenceStatus::Free => ("free".to_string(), "0".to_string()),
        };

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, "licence_status", &status).map_err(|e| e.to_string())?;
    db::set_setting(&conn, "licence_expiry", &expiry).map_err(|e| e.to_string())?;
    Ok(())
}
