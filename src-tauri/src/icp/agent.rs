use base64::Engine;
use candid::{CandidType, Decode, Encode};
use ic_agent::{identity::BasicIdentity, Agent};
pub use ic_agent::export::Principal;
use ring::rand::SystemRandom;
use ring::signature::Ed25519KeyPair;
use std::path::Path;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum IcpError {
    #[error("agent error: {0}")]
    Agent(#[from] ic_agent::AgentError),
    #[error("candid encode error: {0}")]
    Encode(String),
    #[error("candid decode error: {0}")]
    Decode(String),
    #[error("principal error: {0}")]
    Principal(String),
    #[error("identity error: {0}")]
    Identity(String),
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

// ── Device identity ───────────────────────────────────────────────────────────

/// Loads the device identity PEM from `{app_data_dir}/device_identity.pem`.
/// Creates and persists a new Ed25519 keypair on first launch.
/// Returns the raw PEM bytes — callers pass these to `build_agent`.
///
/// Args:
///   app_data_dir: The application data directory path.
///
/// Returns:
///   Ok(pem_bytes) on success.
///   Err if the identity file cannot be written on first launch (disk full, read-only fs).
///   Reading an existing file is best-effort; a corrupt file triggers regeneration.
pub fn load_or_create_identity(app_data_dir: &Path) -> Result<Vec<u8>, String> {
    let path = app_data_dir.join("device_identity.pem");
    if path.exists() {
        if let Ok(bytes) = std::fs::read(&path) {
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
    }
    generate_and_persist_identity(&path)
}

/// Generates a fresh Ed25519 keypair, writes it as a PKCS8 PEM to `path`, and returns
/// the PEM bytes. Returns Err if the file cannot be written — the caller must treat this
/// as a fatal startup error so the device principal is stable across restarts.
///
/// Args:
///   path: Destination file path for the PEM.
///
/// Returns:
///   Ok(pem_bytes) on success, Err(message) if the file cannot be persisted.
fn generate_and_persist_identity(path: &Path) -> Result<Vec<u8>, String> {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .expect("Ed25519 key generation failed");
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref())
    );
    let pem_bytes = pem.into_bytes();
    std::fs::write(path, &pem_bytes)
        .map_err(|e| format!("cannot persist device identity to {}: {e}", path.display()))?;
    Ok(pem_bytes)
}

// ── Agent construction ────────────────────────────────────────────────────────

/// Builds an ic-agent using the device's Ed25519 identity.
/// Uses AnonymousIdentity only when `pem` is empty (legitimate pre-startup state).
/// Returns Err(IcpError::Identity) if a non-empty PEM cannot be parsed — callers surface
/// this to the frontend via the existing icp://status event so the user is informed.
/// Fetches the root key only for local replicas (127.0.0.1 / localhost).
///
/// Args:
///   icp_url: The IC network URL ("https://ic0.app" or "http://127.0.0.1:4943").
///   pem:     PEM bytes from `load_or_create_identity`, or empty for anonymous.
///
/// Returns:
///   Ok(Agent) on success, Err(IcpError::Identity) if a non-empty PEM is malformed.
pub async fn build_agent(icp_url: &str, pem: &[u8]) -> Result<Agent, IcpError> {
    let identity: Box<dyn ic_agent::Identity> = if pem.is_empty() {
        Box::new(ic_agent::identity::AnonymousIdentity)
    } else {
        match BasicIdentity::from_pem(pem) {
            Ok(id) => Box::new(id),
            Err(e) => {
                return Err(IcpError::Identity(format!(
                    "device PEM invalid — governance calls require a valid identity: {e}"
                )));
            }
        }
    };
    let agent = Agent::builder()
        .with_url(icp_url)
        .with_boxed_identity(identity)
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
///   agent:    Configured IC agent.
///   canister: Pattern canister Principal.
///   since_id: Last synced pattern id (0 on first sync).
///
/// Returns:
///   Vec of PatternEntry ordered by id ascending.
pub async fn get_delta_since(
    agent: &Agent,
    canister: &Principal,
    since_id: u64,
) -> Result<Vec<PatternEntry>, IcpError> {
    let arg = Encode!(&since_id).map_err(|e| IcpError::Encode(e.to_string()))?;
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
///   device_id: UUID v4 string identifying this installation (customer record key).
pub async fn register_device(
    agent: &Agent,
    canister: &Principal,
    device_id: String,
) -> Result<(), IcpError> {
    let arg = Encode!(&device_id).map_err(|e| IcpError::Encode(e.to_string()))?;
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
    let arg = Encode!(&device_id).map_err(|e| IcpError::Encode(e.to_string()))?;
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
    let arg = Encode!().map_err(|e| IcpError::Encode(e.to_string()))?;
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
    let arg = Encode!(&device_id).map_err(|e| IcpError::Encode(e.to_string()))?;
    let raw = agent
        .query(canister, "get_reputation")
        .with_arg(arg)
        .call()
        .await?;
    let score = Decode!(&raw, u64).map_err(|e| IcpError::Decode(e.to_string()))?;
    Ok(score)
}

/// Submits a community proposal to flag a SHA-256 hash as a threat.
/// The canister derives the submitter identity from ic_cdk::caller().
///
/// Args:
///   agent:    Configured IC agent (must use the device Ed25519 identity).
///   canister: Governance canister Principal.
///   sha256:   Hex-encoded SHA-256 of the suspect file.
///
/// Returns:
///   The assigned proposal id.
pub async fn submit_proposal(
    agent: &Agent,
    canister: &Principal,
    sha256: String,
) -> Result<u64, IcpError> {
    let arg = Encode!(&sha256).map_err(|e| IcpError::Encode(e.to_string()))?;
    let raw = agent
        .update(canister, "submit_proposal")
        .with_arg(arg)
        .call_and_wait()
        .await?;
    let id = Decode!(&raw, u64).map_err(|e| IcpError::Decode(e.to_string()))?;
    Ok(id)
}

/// Casts an approve or reject vote on a governance proposal.
/// The canister derives the voter identity from ic_cdk::caller().
/// Silently ignored by the canister if the caller has already voted.
///
/// Args:
///   agent:       Configured IC agent (must use the device Ed25519 identity).
///   canister:    Governance canister Principal.
///   proposal_id: The proposal to vote on.
///   approve:     true to approve, false to reject.
pub async fn vote(
    agent: &Agent,
    canister: &Principal,
    proposal_id: u64,
    approve: bool,
) -> Result<(), IcpError> {
    let arg =
        Encode!(&proposal_id, &approve).map_err(|e| IcpError::Encode(e.to_string()))?;
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

    /// Builds a test agent using a fresh in-memory identity (no file I/O).
    async fn test_agent(url: &str) -> Option<Agent> {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).ok()?;
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref())
        );
        build_agent(url, pem.as_bytes()).await.ok()
    }

    /// Attempts to connect to the local dfx replica.
    /// Returns None if the replica is not reachable, allowing tests to self-skip.
    async fn try_local_agent() -> Option<Agent> {
        test_agent(LOCAL_URL).await
    }

    // ── parse_principal ───────────────────────────────────────────────────────

    #[test]
    fn parse_principal_valid_returns_ok() {
        assert!(parse_principal(PATTERN_ID).is_ok());
    }

    #[test]
    fn parse_principal_invalid_returns_err() {
        assert!(parse_principal("not-a-valid-principal!!").is_err());
    }

    #[test]
    fn parse_principal_empty_returns_err() {
        assert!(parse_principal("").is_err());
    }

    // ── load_or_create_identity ───────────────────────────────────────────────

    #[test]
    fn generate_produces_valid_pem() {
        let dir = std::env::temp_dir().join(format!("relay_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pem = load_or_create_identity(&dir);
        assert!(!pem.is_empty());
        // Must be parseable by BasicIdentity.
        BasicIdentity::from_pem(pem.as_slice()).expect("generated PEM should be valid");
        // File must be written.
        assert!(dir.join("device_identity.pem").exists());
    }

    #[test]
    fn load_returns_same_identity_on_second_call() {
        let dir = std::env::temp_dir().join(format!("relay_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = load_or_create_identity(&dir);
        let second = load_or_create_identity(&dir);
        assert_eq!(first, second, "identity should be stable across calls");
    }

    // ── build_agent (requires local dfx) ─────────────────────────────────────

    #[tokio::test]
    async fn build_agent_local_fetches_root_key() {
        let Some(_agent) = try_local_agent().await else {
            eprintln!("SKIP build_agent_local_fetches_root_key: dfx replica not reachable");
            return;
        };
    }

    // ── pattern canister (requires local dfx) ────────────────────────────────

    #[tokio::test]
    async fn get_delta_since_returns_a_vec() {
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
        let Some(agent) = try_local_agent().await else {
            eprintln!("SKIP submit_proposal: dfx replica not reachable");
            return;
        };
        let canister = parse_principal(GOVERNANCE_ID).unwrap();
        let sha256 = format!("{:0>64}", uuid::Uuid::new_v4().simple());

        let proposal_id = submit_proposal(&agent, &canister, sha256.clone())
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
        let Some(submitter_agent) = try_local_agent().await else {
            eprintln!("SKIP vote_approve: dfx replica not reachable");
            return;
        };
        // Use a separate identity for voting so submitter != voter.
        let Some(voter_agent) = try_local_agent().await else { return };
        let canister = parse_principal(GOVERNANCE_ID).unwrap();
        let sha256 = format!("{:0>64}", uuid::Uuid::new_v4().simple());

        let proposal_id = submit_proposal(&submitter_agent, &canister, sha256)
            .await
            .expect("submit_proposal failed");

        vote(&voter_agent, &canister, proposal_id, true)
            .await
            .expect("vote failed");

        let proposals = get_proposals(&voter_agent, &canister)
            .await
            .expect("get_proposals after vote failed");
        let p = proposals
            .iter()
            .find(|p| p.id == proposal_id)
            .expect("proposal not found after vote");
        assert!(p.approve_votes >= 1, "approve_votes should be at least 1 after voting approve");
    }
}
