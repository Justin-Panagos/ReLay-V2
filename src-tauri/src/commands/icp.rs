use crate::db::{self, DbState};
use crate::icp::agent::{self, Proposal};
use crate::icp::{ConfigState, DeviceIdentityState};
use crate::pro::{self, LicenceCacheState};
use tauri::State;

/// Returns the Unix timestamp of the last successful ICP pattern sync and
/// the total number of patterns in the local cache.
/// last_sync_ts is 0 when a sync has never completed successfully.
///
/// Args:
///   db: Tauri-managed database state.
///
/// Returns:
///   Ok(JSON { last_sync_ts: u64, pattern_count: u64 }) on success, Err on DB failure.
#[tauri::command]
pub fn get_pattern_sync_info(db: State<'_, DbState>) -> Result<serde_json::Value, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let last_sync = db::get_setting(&conn, "last_pattern_sync")
        .map_err(|e| e.to_string())?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let count = db::count_icp_patterns(&conn).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "last_sync_ts": last_sync, "pattern_count": count }))
}

/// Submits a quarantined file's SHA-256 to the governance canister for community review.
/// The canister derives the submitter identity from the device's Ed25519 principal.
///
/// Args:
///   quarantine_id: Row id from the quarantine table.
///   db:            Tauri-managed database state.
///   config:        Tauri-managed ICP config state.
///   identity:      Tauri-managed device identity state (PEM bytes).
///
/// Returns:
///   Ok(proposal_id) on success, Err(message) on failure.
#[tauri::command]
pub async fn submit_zero_day(
    quarantine_id: i64,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    identity: State<'_, DeviceIdentityState>,
) -> Result<u64, String> {
    let sha256 = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let record = db::get_quarantine_by_id(&conn, quarantine_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("quarantine entry {quarantine_id} not found"))?;
        record.sha256
    };

    let pem = identity.0.clone();
    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url, &pem)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::submit_proposal(&a, &governance_id, sha256)
        .await
        .map_err(|e| e.to_string())
}

/// Returns all governance proposals from the ICP canister.
///
/// Args:
///   config:   Tauri-managed ICP config state.
///   identity: Tauri-managed device identity state (PEM bytes).
///
/// Returns:
///   Vec of Proposal on success, Err(message) on failure.
#[tauri::command]
pub async fn get_proposals(
    config: State<'_, ConfigState>,
    identity: State<'_, DeviceIdentityState>,
) -> Result<Vec<Proposal>, String> {
    let pem = identity.0.clone();
    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url, &pem)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::get_proposals(&a, &governance_id)
        .await
        .map_err(|e| e.to_string())
}

/// Returns the reputation score for the current device from the governance canister.
///
/// Args:
///   db:       Tauri-managed database state (used to read device_id from settings).
///   config:   Tauri-managed ICP config state.
///   identity: Tauri-managed device identity state (PEM bytes).
///
/// Returns:
///   Ok(score) on success, Err(message) on failure.
#[tauri::command]
pub async fn get_reputation(
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
    identity: State<'_, DeviceIdentityState>,
) -> Result<u64, String> {
    let device_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| "anonymous".to_string())
    };

    let pem = identity.0.clone();
    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url, &pem)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::get_reputation(&a, &governance_id, device_id)
        .await
        .map_err(|e| e.to_string())
}

/// Casts an approve or reject vote on a pending governance proposal.
/// The canister derives the voter identity from the device's Ed25519 principal.
///
/// Args:
///   proposal_id: The proposal to vote on.
///   approve:     true to approve, false to reject.
///   config:      Tauri-managed ICP config state.
///   identity:    Tauri-managed device identity state (PEM bytes).
///   cache:       Tauri-managed in-memory licence cache.
///
/// Returns:
///   Ok(()) on success, Err("Pro subscription required") if not Pro, Err(message) on failure.
#[tauri::command]
pub async fn vote_proposal(
    proposal_id: u64,
    approve: bool,
    config: State<'_, ConfigState>,
    identity: State<'_, DeviceIdentityState>,
    cache: State<'_, LicenceCacheState>,
) -> Result<(), String> {
    pro::require_pro(&cache)?;
    let pem = identity.0.clone();
    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url, &pem)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::vote(&a, &governance_id, proposal_id, approve)
        .await
        .map_err(|e| e.to_string())
}
