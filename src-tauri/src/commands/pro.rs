use crate::db::{self, DbState};
use crate::icp::{agent, ConfigState, DeviceIdentityState};
use crate::pro::{self, LicenceCacheState};
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

/// Returns the current licence status, expiry, and device ID.
/// Status and expiry are read from the in-memory licence cache (ICP-verified).
/// Device ID is read from SQLite (stable identifier, not security-sensitive).
///
/// Args:
///   cache: Tauri-managed in-memory licence cache.
///   db:    Tauri-managed database state (for device_id only).
///
/// Returns:
///   ProStatus with values read from the cache and settings table.
#[tauri::command]
pub fn get_pro_status(
    cache: State<'_, LicenceCacheState>,
    db: State<'_, DbState>,
) -> Result<ProStatus, String> {
    let (status, expiry_timestamp) = {
        let lock = cache.0.lock().unwrap_or_else(|p| p.into_inner());
        let exp = if lock.expiry > 0 { Some(lock.expiry) } else { None };
        (lock.status.clone(), exp)
    };
    let conn = db.0.lock().map_err(|e| e.to_string())?;
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
///   email:  The user's email address for the Paystack payment page.
///   db:     Tauri-managed database state (reads device_id).
///   config: Tauri-managed ICP config state (reads worker_url).
///   client: Shared reqwest HTTP client.
///   app:    Tauri app handle (used for shell::open).
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
#[tauri::command]
pub async fn open_upgrade_page(
    email: String,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if email.is_empty() {
        return Err("email is required to start checkout".to_string());
    }

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

    // Persist email so cancel_subscription can include it for ownership verification.
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::set_setting(&conn, "pro_email", &email).map_err(|e| e.to_string())?;
    }

    let resp = client
        .post(format!("{worker_url}/init-checkout"))
        .json(&serde_json::json!({ "device_id": device_id, "email": email }))
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

/// Cancels the user's active Pro subscription via the Cloudflare Worker.
/// The Worker calls the Paystack disable API; the resulting subscription.disable webhook
/// will revoke the licence on ICP. This command also immediately updates the local
/// in-memory cache to "free" so the UI reflects the change without waiting for the webhook.
///
/// Args:
///   cache:  Tauri-managed in-memory licence cache.
///   db:     Tauri-managed database state (reads device_id and worker_url).
///   config: Tauri-managed ICP config state (reads worker_url).
///   client: Shared reqwest HTTP client.
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
#[tauri::command]
pub async fn cancel_subscription(
    cache: State<'_, LicenceCacheState>,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
) -> Result<(), String> {
    let (device_id, pro_email) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let did = db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "device_id not set".to_string())?;
        let em = db::get_setting(&conn, "pro_email")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        (did, em)
    };

    let worker_url = config
        .0
        .worker_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "worker_url not set in config.toml".to_string())?
        .to_string();

    let resp = client
        .post(format!("{worker_url}/cancel-subscription"))
        .json(&serde_json::json!({ "device_id": device_id, "email": pro_email }))
        .send()
        .await
        .map_err(|e| format!("could not reach payment server: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("cancellation failed: {}", resp.status()));
    }

    // Update cache immediately — the webhook will also revoke via ICP shortly after.
    pro::update_licence_cache(&cache, "free", 0);
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, "licence_status", "free").map_err(|e| e.to_string())?;
    db::set_setting(&conn, "licence_expiry", "0").map_err(|e| e.to_string())?;
    Ok(())
}

/// Re-checks the licence status against the ICP Identity Canister, updates the
/// in-memory licence cache (the authoritative source for gate checks), and persists
/// the result to SQLite for the next startup.
/// Used by the "Re-check" button in Settings → Pro after a user upgrades.
///
/// Args:
///   cache:  Tauri-managed in-memory licence cache.
///   db:     Tauri-managed database state.
///   config: Tauri-managed ICP config state.
///
/// Returns:
///   Ok(()) on success. ICP network errors are returned as Err strings.
#[tauri::command]
pub async fn recheck_licence(
    cache: State<'_, LicenceCacheState>,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    identity: State<'_, DeviceIdentityState>,
) -> Result<(), String> {
    let device_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "device_id not set".to_string())?
    };

    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url, &identity.0)
        .await
        .map_err(|e| e.to_string())?;
    let identity_id =
        agent::parse_principal(&icp.canisters.identity).map_err(|e| e.to_string())?;

    let (status, expiry_u64) =
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
                    ("pro".to_string(), expiry_timestamp)
                } else {
                    ("free".to_string(), 0u64)
                }
            }
            agent::LicenceStatus::Free => ("free".to_string(), 0u64),
        };

    // Update in-memory cache first — gate checks read from here.
    pro::update_licence_cache(&cache, &status, expiry_u64);

    // Persist to SQLite as a fallback seed for future restarts.
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, "licence_status", &status).map_err(|e| e.to_string())?;
    db::set_setting(&conn, "licence_expiry", &expiry_u64.to_string())
        .map_err(|e| e.to_string())?;
    Ok(())
}
