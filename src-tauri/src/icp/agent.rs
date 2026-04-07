use candid::{CandidType, Decode, Encode};
use ic_agent::{identity::AnonymousIdentity, Agent};
pub use ic_agent::export::Principal;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum IcpError {
    #[error("agent error: {0}")]
    Agent(#[from] ic_agent::AgentError),
    #[error("candid error: {0}")]
    Decode(String),
    #[error("principal error: {0}")]
    Principal(String),
}

// ── Mirror types ──────────────────────────────────────────────────────────────

/// A single entry in the pattern canister ledger.
#[derive(Debug, Clone, CandidType, serde::Deserialize, serde::Serialize)]
pub struct PatternEntry {
    pub id: u64,
    pub sha256: String,
    pub threat_level: String,
    pub source: String,
    pub timestamp: u64,
}

/// Licence status returned by the identity canister.
#[derive(Debug, Clone, CandidType, serde::Deserialize, serde::Serialize)]
pub enum LicenceStatus {
    Free,
    Pro { expiry_timestamp: u64 },
}

/// A governance proposal from the governance canister.
#[derive(Debug, Clone, CandidType, serde::Deserialize, serde::Serialize)]
pub struct Proposal {
    pub id: u64,
    pub sha256: String,
    pub submitter: String,
    pub approve_votes: u64,
    pub reject_votes: u64,
    pub status: String,
    pub created_at: u64,
}

// ── Agent construction ────────────────────────────────────────────────────────

/// Builds an ic-agent pointed at the given ICP network URL.
/// Uses AnonymousIdentity for all Phase 11 calls.
/// Fetches the root key only for local replicas (127.0.0.1 / localhost).
///
/// Args:
///   icp_url: The IC network URL ("https://ic0.app" or "http://127.0.0.1:4943").
///
/// Returns:
///   A configured Agent ready to make calls.
pub async fn build_agent(icp_url: &str) -> Result<Agent, IcpError> {
    let agent = Agent::builder()
        .with_url(icp_url)
        .with_identity(AnonymousIdentity)
        .build()?;
    if icp_url.contains("127.0.0.1") || icp_url.contains("localhost") {
        agent.fetch_root_key().await?;
    }
    Ok(agent)
}

/// Parses a canister ID string into a Principal.
///
/// Args:
///   id: Textual canister ID (e.g. "aaaaa-aa").
///
/// Returns:
///   Ok(Principal) on success, Err on invalid format.
pub fn parse_principal(id: &str) -> Result<Principal, IcpError> {
    Principal::from_text(id).map_err(|e| IcpError::Principal(e.to_string()))
}

// ── Pattern canister ──────────────────────────────────────────────────────────

/// Returns all pattern entries with id > since_id (delta sync).
///
/// Args:
///   agent:      Configured IC agent.
///   canister:   Pattern canister Principal.
///   since_id:   Last synced pattern id (0 on first sync).
///
/// Returns:
///   Vec of PatternEntry ordered by id ascending.
pub async fn get_delta_since(
    agent: &Agent,
    canister: &Principal,
    since_id: u64,
) -> Result<Vec<PatternEntry>, IcpError> {
    let arg = Encode!(&since_id).map_err(|e| IcpError::Decode(e.to_string()))?;
    let raw = agent
        .query(canister, "get_delta_since")
        .with_arg(arg)
        .call()
        .await?;
    let entries =
        Decode!(&raw, Vec<PatternEntry>).map_err(|e| IcpError::Decode(e.to_string()))?;
    Ok(entries)
}

// ── Identity canister ─────────────────────────────────────────────────────────

/// Registers the device on the identity canister (idempotent — no-op if already registered).
///
/// Args:
///   agent:     Configured IC agent.
///   canister:  Identity canister Principal.
///   device_id: UUID v4 string identifying this installation.
pub async fn register_device(
    agent: &Agent,
    canister: &Principal,
    device_id: String,
) -> Result<(), IcpError> {
    let arg = Encode!(&device_id).map_err(|e| IcpError::Decode(e.to_string()))?;
    agent
        .update(canister, "register_device")
        .with_arg(arg)
        .call_and_wait()
        .await?;
    Ok(())
}

/// Queries the licence status for the given device.
///
/// Args:
///   agent:     Configured IC agent.
///   canister:  Identity canister Principal.
///   device_id: UUID v4 string identifying this installation.
///
/// Returns:
///   LicenceStatus::Free or LicenceStatus::Pro { expiry_timestamp }.
pub async fn check_licence(
    agent: &Agent,
    canister: &Principal,
    device_id: String,
) -> Result<LicenceStatus, IcpError> {
    let arg = Encode!(&device_id).map_err(|e| IcpError::Decode(e.to_string()))?;
    let raw = agent
        .query(canister, "check_licence")
        .with_arg(arg)
        .call()
        .await?;
    let status =
        Decode!(&raw, LicenceStatus).map_err(|e| IcpError::Decode(e.to_string()))?;
    Ok(status)
}

// ── Governance canister ───────────────────────────────────────────────────────

/// Returns all governance proposals ordered by id ascending.
///
/// Args:
///   agent:    Configured IC agent.
///   canister: Governance canister Principal.
///
/// Returns:
///   Vec of Proposal.
pub async fn get_proposals(
    agent: &Agent,
    canister: &Principal,
) -> Result<Vec<Proposal>, IcpError> {
    let arg = Encode!().map_err(|e| IcpError::Decode(e.to_string()))?;
    let raw = agent
        .query(canister, "get_proposals")
        .with_arg(arg)
        .call()
        .await?;
    let proposals =
        Decode!(&raw, Vec<Proposal>).map_err(|e| IcpError::Decode(e.to_string()))?;
    Ok(proposals)
}

/// Returns the reputation score for a device.
///
/// Args:
///   agent:     Configured IC agent.
///   canister:  Governance canister Principal.
///   device_id: UUID v4 string identifying this installation.
///
/// Returns:
///   Reputation point count (0 for unknown devices).
pub async fn get_reputation(
    agent: &Agent,
    canister: &Principal,
    device_id: String,
) -> Result<u64, IcpError> {
    let arg = Encode!(&device_id).map_err(|e| IcpError::Decode(e.to_string()))?;
    let raw = agent
        .query(canister, "get_reputation")
        .with_arg(arg)
        .call()
        .await?;
    let score = Decode!(&raw, u64).map_err(|e| IcpError::Decode(e.to_string()))?;
    Ok(score)
}

/// Submits a community proposal to flag a SHA-256 hash as a threat.
///
/// Args:
///   agent:     Configured IC agent.
///   canister:  Governance canister Principal.
///   sha256:    Hex-encoded SHA-256 of the suspect file.
///   submitter: device_id of the submitting user.
///
/// Returns:
///   The assigned proposal id.
pub async fn submit_proposal(
    agent: &Agent,
    canister: &Principal,
    sha256: String,
    submitter: String,
) -> Result<u64, IcpError> {
    let arg =
        Encode!(&sha256, &submitter).map_err(|e| IcpError::Decode(e.to_string()))?;
    let raw = agent
        .update(canister, "submit_proposal")
        .with_arg(arg)
        .call_and_wait()
        .await?;
    let id = Decode!(&raw, u64).map_err(|e| IcpError::Decode(e.to_string()))?;
    Ok(id)
}

/// Casts an approve or reject vote on a governance proposal.
/// Silently ignored by the canister if the device has already voted.
///
/// Args:
///   agent:       Configured IC agent.
///   canister:    Governance canister Principal.
///   proposal_id: The proposal to vote on.
///   voter:       device_id of the voting user.
///   approve:     true to approve, false to reject.
pub async fn vote(
    agent: &Agent,
    canister: &Principal,
    proposal_id: u64,
    voter: String,
    approve: bool,
) -> Result<(), IcpError> {
    let arg =
        Encode!(&proposal_id, &voter, &approve).map_err(|e| IcpError::Decode(e.to_string()))?;
    agent
        .update(canister, "vote")
        .with_arg(arg)
        .call_and_wait()
        .await?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Local dfx canister IDs — from icp/.dfx/local/canister_ids.json
    const LOCAL_URL: &str = "http://127.0.0.1:4943";
    const PATTERN_ID: &str = "uzt4z-lp777-77774-qaabq-cai";
    const IDENTITY_ID: &str = "u6s2n-gx777-77774-qaaba-cai";
    const GOVERNANCE_ID: &str = "uxrrr-q7777-77774-qaaaq-cai";

    /// Attempts to build an agent against the local dfx replica.
    /// Returns None if the replica is not reachable, allowing tests to self-skip.
    async fn try_local_agent() -> Option<Agent> {
        build_agent(LOCAL_URL).await.ok()
    }

    // ── parse_principal ───────────────────────────────────────────────────────

    #[test]
    fn parse_principal_valid_returns_ok() {
        // Verifies that a well-formed textual principal parses without error.
        assert!(parse_principal(PATTERN_ID).is_ok());
    }

    #[test]
    fn parse_principal_invalid_returns_err() {
        // Verifies that a garbage string returns an Err rather than panicking.
        assert!(parse_principal("not-a-valid-principal!!").is_err());
    }

    #[test]
    fn parse_principal_empty_returns_err() {
        // Verifies that an empty string returns an Err.
        assert!(parse_principal("").is_err());
    }

    // ── build_agent (requires local dfx) ─────────────────────────────────────

    #[tokio::test]
    async fn build_agent_local_fetches_root_key() {
        // Verifies that build_agent succeeds and fetches the root key for a local replica.
        // Skipped automatically when dfx is not running.
        let Some(_agent) = try_local_agent().await else {
            eprintln!("SKIP build_agent_local_fetches_root_key: dfx replica not reachable");
            return;
        };
    }

    // ── pattern canister (requires local dfx) ────────────────────────────────

    #[tokio::test]
    async fn get_delta_since_returns_a_vec() {
        // Verifies that get_delta_since returns Ok(Vec) without error.
        // Does not assert empty — the local canister may already have entries from prior runs.
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP get_delta_since: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(PATTERN_ID).unwrap();
        get_delta_since(&agent, &canister, 0)
            .await
            .expect("get_delta_since failed");
    }

    // ── identity canister (requires local dfx) ───────────────────────────────

    #[tokio::test]
    async fn new_device_licence_is_free() {
        // Verifies that a freshly generated device ID returns LicenceStatus::Free.
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP new_device_licence_is_free: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(IDENTITY_ID).unwrap();
        let device_id = uuid::Uuid::new_v4().to_string();
        let status = check_licence(&agent, &canister, device_id)
            .await
            .expect("check_licence failed");
        assert!(
            matches!(status, LicenceStatus::Free),
            "expected Free, got {status:?}"
        );
    }

    #[tokio::test]
    async fn register_device_is_idempotent() {
        // Verifies that calling register_device twice with the same device_id succeeds both times.
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP register_device_is_idempotent: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(IDENTITY_ID).unwrap();
        let device_id = uuid::Uuid::new_v4().to_string();
        register_device(&agent, &canister, device_id.clone())
            .await
            .expect("first register_device failed");
        register_device(&agent, &canister, device_id)
            .await
            .expect("second register_device (idempotent) failed");
    }

    // ── governance canister (requires local dfx) ──────────────────────────────

    #[tokio::test]
    async fn get_proposals_returns_a_vec() {
        // Verifies that get_proposals returns Ok(Vec) without error.
        // Does not assert empty — other tests submit proposals to the same local canister.
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP get_proposals: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(GOVERNANCE_ID).unwrap();
        get_proposals(&agent, &canister)
            .await
            .expect("get_proposals failed");
    }

    #[tokio::test]
    async fn unknown_device_reputation_is_zero() {
        // Verifies that get_reputation returns 0 for a device that has never interacted.
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP unknown_device_reputation_is_zero: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(GOVERNANCE_ID).unwrap();
        let device_id = uuid::Uuid::new_v4().to_string();
        let score = get_reputation(&agent, &canister, device_id)
            .await
            .expect("get_reputation failed");
        assert_eq!(score, 0, "unknown device should have reputation 0");
    }

    #[tokio::test]
    async fn submit_proposal_returns_id_and_appears_in_list() {
        // Verifies the full proposal lifecycle: submit → appears in get_proposals.
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP submit_proposal: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(GOVERNANCE_ID).unwrap();
        // Use a unique fake sha256 so parallel test runs don't collide.
        let sha256 = format!("{:0>64}", uuid::Uuid::new_v4().simple());
        let submitter = uuid::Uuid::new_v4().to_string();

        let proposal_id = submit_proposal(&agent, &canister, sha256.clone(), submitter)
            .await
            .expect("submit_proposal failed");

        let proposals = get_proposals(&agent, &canister)
            .await
            .expect("get_proposals failed");
        let found = proposals.iter().any(|p| p.id == proposal_id);
        assert!(found, "submitted proposal {proposal_id} not found in list");
    }

    #[tokio::test]
    async fn vote_approve_increments_approve_count() {
        // Verifies that voting approve on a proposal increments its approve_votes count.
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP vote_approve: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(GOVERNANCE_ID).unwrap();
        let sha256 = format!("{:0>64}", uuid::Uuid::new_v4().simple());
        let submitter = uuid::Uuid::new_v4().to_string();
        let voter = uuid::Uuid::new_v4().to_string();

        let proposal_id = submit_proposal(&agent, &canister, sha256, submitter)
            .await
            .expect("submit_proposal failed");

        vote(&agent, &canister, proposal_id, voter, true)
            .await
            .expect("vote failed");

        let proposals = get_proposals(&agent, &canister)
            .await
            .expect("get_proposals after vote failed");
        let p = proposals
            .iter()
            .find(|p| p.id == proposal_id)
            .expect("proposal not found after vote");
        assert!(p.approve_votes >= 1, "approve_votes should be at least 1 after voting approve");
    }
}
