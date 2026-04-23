use crate::db::{self, DbState};
use crate::icp::ConfigState;
use tauri::{Manager, State};

/// Stored developer API credentials returned to the frontend.
#[derive(serde::Serialize)]
pub struct ApiCredentials {
    /// Email used to purchase the key or snapshot.
    pub email: String,
    /// The API key or snapshot token; empty string when none stored.
    pub key: String,
    /// "none" | "snapshot" | "monthly"
    pub plan: String,
    /// Unix seconds expiry; 0 when not set.
    pub expiry: u64,
    /// Pre-built /api/delta endpoint URL for subscription plans; empty for snapshot/none.
    pub endpoint_url: String,
}

/// Result of a single poll attempt returned to the frontend.
#[derive(serde::Serialize)]
pub struct DevStatus {
    /// True when a key or token was found (payment confirmed).
    pub found: bool,
    /// The API key or snapshot token.
    pub key: String,
    /// "snapshot" | "monthly"
    pub plan: String,
    /// Unix seconds expiry.
    pub expiry: u64,
    /// Pre-built /api/delta endpoint URL; empty for snapshot.
    pub endpoint_url: String,
}

/// Reads stored developer credentials from SQLite settings.
/// Returns ApiCredentials with plan="none" and empty key when nothing is stored.
///
/// Args:
///   db: Tauri-managed database state.
///
/// Returns:
///   ApiCredentials with values from the settings table.
#[tauri::command]
pub fn get_stored_api_credentials(
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
) -> Result<ApiCredentials, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let email  = db::get_setting(&conn, "api_email") .map_err(|e| e.to_string())?.unwrap_or_default();
    let key    = db::get_setting(&conn, "api_key")   .map_err(|e| e.to_string())?.unwrap_or_default();
    let plan   = db::get_setting(&conn, "api_plan")  .map_err(|e| e.to_string())?.unwrap_or_else(|| "none".to_string());
    let expiry = db::get_setting(&conn, "api_expiry").map_err(|e| e.to_string())?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let endpoint_url = if plan != "none" && plan != "snapshot" && !key.is_empty() {
        build_endpoint_url(&config, &key)
    } else {
        String::new()
    };

    Ok(ApiCredentials { email, key, plan, expiry, endpoint_url })
}

/// Initiates a one-time snapshot checkout via the Cloudflare Worker and opens
/// the Paystack authorization URL in the system browser.
///
/// Args:
///   email:  The user's email address for the Paystack payment page.
///   config: Tauri-managed ICP config state (reads worker_url).
///   client: Shared reqwest HTTP client.
///   app:    Tauri app handle (used for shell::open).
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
#[tauri::command]
pub async fn api_snapshot_checkout(
    email: String,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if email.is_empty() {
        return Err("email is required".to_string());
    }
    let worker_url = get_worker_url(&config)?;
    let resp = client
        .post(format!("{worker_url}/snapshot-checkout"))
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
        .map_err(|e| format!("could not reach payment server: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("payment server returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let url = json["authorization_url"]
        .as_str()
        .ok_or_else(|| "no authorization_url in response".to_string())?
        .to_string();
    tauri::api::shell::open(&app.shell_scope(), url, None).map_err(|e| e.to_string())
}

/// Initiates an API feed subscription checkout via the Cloudflare Worker and opens
/// the Paystack authorization URL in the system browser.
///
/// Args:
///   plan:   "monthly"
///   email:  The user's email address for the Paystack payment page.
///   config: Tauri-managed ICP config state (reads worker_url).
///   client: Shared reqwest HTTP client.
///   app:    Tauri app handle (used for shell::open).
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
#[tauri::command]
pub async fn api_key_checkout(
    plan: String,
    email: String,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if email.is_empty() {
        return Err("email is required".to_string());
    }
    let worker_url = get_worker_url(&config)?;
    let resp = client
        .post(format!("{worker_url}/api-checkout"))
        .json(&serde_json::json!({ "plan": plan, "email": email }))
        .send()
        .await
        .map_err(|e| format!("could not reach payment server: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("payment server returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let url = json["authorization_url"]
        .as_str()
        .ok_or_else(|| "no authorization_url in response".to_string())?
        .to_string();
    tauri::api::shell::open(&app.shell_scope(), url, None).map_err(|e| e.to_string())
}

/// Polls the Cloudflare Worker to check whether a payment has completed for the
/// given email. Checks /api/key-status then /api/snap-status. Saves credentials
/// to SQLite when found.
///
/// Returns DevStatus { found: false, ... } when payment is still pending — this
/// is not an error, the caller should retry.
///
/// Args:
///   email:  Email address used at checkout.
///   config: Tauri-managed ICP config state (reads worker_url).
///   client: Shared reqwest HTTP client.
///   db:     Tauri-managed database state (persists credentials on success).
///
/// Returns:
///   DevStatus with found=true when payment is confirmed, found=false when still waiting.
#[tauri::command]
pub async fn poll_developer_status(
    email: String,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
    db: State<'_, DbState>,
) -> Result<DevStatus, String> {
    if email.is_empty() {
        return Err("email is required".to_string());
    }
    let worker_url = get_worker_url(&config)?;

    // Try API key subscription first.
    let key_resp = client
        .get(format!("{worker_url}/api/key-status"))
        .query(&[("email", &email)])
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;

    if key_resp.status().as_u16() == 200 {
        let json: serde_json::Value = key_resp.json().await.map_err(|e| e.to_string())?;
        let api_key = json["apiKey"].as_str().unwrap_or("").to_string();
        if api_key.len() != 64 || !api_key.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("worker returned malformed API key — try again".to_string());
        }
        let plan    = json["plan"].as_str().unwrap_or("monthly").to_string();
        let expiry  = json["expiry"].as_u64().unwrap_or(0);
        persist_credentials(&db, &email, &api_key, &plan, expiry)?;
        let endpoint_url = build_endpoint_url(&config, &api_key);
        return Ok(DevStatus { found: true, key: api_key, plan, expiry, endpoint_url });
    }

    // Try snapshot token.
    let snap_resp = client
        .get(format!("{worker_url}/api/snap-status"))
        .query(&[("email", &email)])
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;

    if snap_resp.status().as_u16() == 200 {
        let json: serde_json::Value = snap_resp.json().await.map_err(|e| e.to_string())?;
        let token  = json["token"].as_str().unwrap_or("").to_string();
        if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("worker returned malformed snapshot token — try again".to_string());
        }
        let expiry = json["expiry"].as_u64().unwrap_or(0);
        persist_credentials(&db, &email, &token, "snapshot", expiry)?;
        return Ok(DevStatus {
            found: true,
            key: token,
            plan: "snapshot".to_string(),
            expiry,
            endpoint_url: String::new(),
        });
    }

    // Payment not yet confirmed — not an error, caller should retry.
    Ok(DevStatus {
        found: false,
        key: String::new(),
        plan: String::new(),
        expiry: 0,
        endpoint_url: String::new(),
    })
}

/// Cancels the user's active API feed subscription via the Cloudflare Worker
/// and clears the stored credentials from SQLite on success.
///
/// Args:
///   email:  Email address associated with the subscription.
///   db:     Tauri-managed database state.
///   config: Tauri-managed ICP config state (reads worker_url).
///   client: Shared reqwest HTTP client.
///
/// Returns:
///   Ok(()) on success, Err(message) on any failure.
#[tauri::command]
pub async fn cancel_api_subscription(
    email: String,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
) -> Result<(), String> {
    let worker_url = get_worker_url(&config)?;
    let resp = client
        .post(format!("{worker_url}/cancel-api-subscription"))
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
        .map_err(|e| format!("could not reach payment server: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("cancellation failed: {}", resp.status()));
    }
    clear_credentials(&db)
}

/// Downloads the full threat database using the stored snapshot token and saves
/// it to disk at the given destination directory.
///
/// Args:
///   format:      "json" | "yara" | "csv"
///   destination: Absolute path to the directory where the file will be saved.
///   db:          Tauri-managed database state (reads the snapshot token).
///   config:      Tauri-managed ICP config state (reads worker_url).
///   client:      Shared reqwest HTTP client.
///
/// Returns:
///   Ok(()) on success, Err(message) on network or file write failure.
///   Returns a specific error on 403 (token already used).
#[tauri::command]
pub async fn download_threat_export(
    format: String,
    destination: String,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    client: State<'_, reqwest::Client>,
) -> Result<(), String> {
    let (token, api_expiry) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let t = db::get_setting(&conn, "api_key")
            .map_err(|e| e.to_string())?
            .filter(|k| !k.is_empty())
            .ok_or_else(|| "no snapshot token stored".to_string())?;
        let e = db::get_setting(&conn, "api_expiry")
            .map_err(|e| e.to_string())?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        (t, e)
    };

    if api_expiry > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now > api_expiry {
            return Err("credential has expired — renew your subscription".to_string());
        }
    }

    let worker_url = get_worker_url(&config)?;

    let resp = client
        .get(format!("{worker_url}/api/export"))
        .query(&[("token", &token), ("format", &format)])
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;

    if resp.status().as_u16() == 403 {
        // Token already used — clear local credentials so the UI reverts.
        clear_credentials(&db)?;
        return Err("Token already used — credentials cleared".to_string());
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("{status}: {body}"));
    }

    let ext = match format.as_str() {
        "yara" => "yar",
        "csv"  => "csv",
        _      => "json",
    };
    let filename = format!("relay_threats.{ext}");
    let path = std::path::PathBuf::from(&destination).join(&filename);

    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    std::fs::write(&path, &bytes).map_err(|e| format!("could not write file: {e}"))
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Reads and validates worker_url from the ICP config.
///
/// Args:
///   config: Tauri-managed ICP config state.
///
/// Returns:
///   The worker URL string, or Err if not configured.
fn get_worker_url(config: &State<'_, ConfigState>) -> Result<String, String> {
    config
        .0
        .worker_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "worker_url not set in config.toml — add it and restart".to_string())
}

/// Builds a ready-to-use /api/delta endpoint URL for a given API key.
///
/// Args:
///   config: Tauri-managed ICP config state.
///   key:    The API key to embed in the URL.
///
/// Returns:
///   Fully formed URL string, or empty string if worker_url is not configured.
fn build_endpoint_url(config: &State<'_, ConfigState>, key: &str) -> String {
    config
        .0
        .worker_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|base| format!("{base}/api/delta?since=0&api_key={key}&format=json"))
        .unwrap_or_default()
}

/// Persists developer credentials to the SQLite settings table.
///
/// Args:
///   db:     Tauri-managed database state.
///   email:  Email associated with the purchase.
///   key:    API key or snapshot token.
///   plan:   Plan identifier string.
///   expiry: Unix seconds expiry timestamp.
///
/// Returns:
///   Ok(()) on success, Err on database failure.
fn persist_credentials(
    db: &State<'_, DbState>,
    email: &str,
    key: &str,
    plan: &str,
    expiry: u64,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_email",  email).map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_key",    key).map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_plan",   plan).map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_expiry", &expiry.to_string()).map_err(|e| e.to_string())
}

/// Clears all developer credentials from the SQLite settings table.
///
/// Args:
///   db: Tauri-managed database state.
///
/// Returns:
///   Ok(()) on success, Err on database failure.
fn clear_credentials(db: &State<'_, DbState>) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_email",  "").map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_key",    "").map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_plan",   "none").map_err(|e| e.to_string())?;
    db::set_setting(&conn, "api_expiry", "0").map_err(|e| e.to_string())
}
