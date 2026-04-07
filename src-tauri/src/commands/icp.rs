use crate::db::{self, DbState};
use crate::icp::agent::{self, Proposal};
use crate::icp::ConfigState;
use tauri::State;

/// Submits a quarantined file's SHA-256 to the governance canister for community review.
/// Looks up the sha256 from the quarantine table, then calls governance::submit_proposal.
///
/// Args:
///   quarantine_id: Row id from the quarantine table.
///   db:            Tauri-managed database state.
///   config:        Tauri-managed ICP config state.
///
/// Returns:
///   Ok(proposal_id) on success, Err(message) on failure.
#[tauri::command]
pub async fn submit_zero_day(
    quarantine_id: i64,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
) -> Result<u64, String> {
    // Look up sha256 and device_id — lock, read, release before any await.
    let (sha256, device_id) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let record = db::get_quarantine_by_id(&conn, quarantine_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("quarantine entry {quarantine_id} not found"))?;
        let device_id = db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| "anonymous".to_string());
        (record.sha256, device_id)
    };

    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::submit_proposal(&a, &governance_id, sha256, device_id)
        .await
        .map_err(|e| e.to_string())
}

/// Returns all governance proposals from the ICP canister.
///
/// Args:
///   config: Tauri-managed ICP config state.
///
/// Returns:
///   Vec of Proposal on success, Err(message) on failure.
#[tauri::command]
pub async fn get_proposals(config: State<'_, ConfigState>) -> Result<Vec<Proposal>, String> {
    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::get_proposals(&a, &governance_id)
        .await
        .map_err(|e| e.to_string())
}

/// Returns the reputation score for the current device.
///
/// Args:
///   db:     Tauri-managed database state (used to read device_id from settings).
///   config: Tauri-managed ICP config state.
///
/// Returns:
///   Ok(score) on success, Err(message) on failure.
#[tauri::command]
pub async fn get_reputation(
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
) -> Result<u64, String> {
    let device_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| "anonymous".to_string())
    };

    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::get_reputation(&a, &governance_id, device_id)
        .await
        .map_err(|e| e.to_string())
}

/// Casts an approve or reject vote on a pending governance proposal.
///
/// Args:
///   proposal_id: The proposal to vote on.
///   approve:     true to approve, false to reject.
///   db:          Tauri-managed database state (used to read device_id from settings).
///   config:      Tauri-managed ICP config state.
///
/// Returns:
///   Ok(()) on success, Err(message) on failure.
#[tauri::command]
pub async fn vote_proposal(
    proposal_id: u64,
    approve: bool,
    db: State<'_, DbState>,
    config: State<'_, ConfigState>,
) -> Result<(), String> {
    let device_id = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "device_id")
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| "anonymous".to_string())
    };

    let icp = &config.0;
    let a = agent::build_agent(&icp.icp_url)
        .await
        .map_err(|e| e.to_string())?;
    let governance_id =
        agent::parse_principal(&icp.canisters.governance).map_err(|e| e.to_string())?;

    agent::vote(&a, &governance_id, proposal_id, device_id, approve)
        .await
        .map_err(|e| e.to_string())
}
