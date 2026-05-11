use candid::{CandidType, Decode, Encode};
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::storable::Bound;
use ic_stable_structures::{DefaultMemoryImpl, StableBTreeMap, Storable};
use serde::Deserialize;
use std::borrow::Cow;
use std::cell::RefCell;

type Memory = VirtualMemory<DefaultMemoryImpl>;

/// Number of votes required to resolve a proposal as approved or rejected.
const VOTE_THRESHOLD: u64 = 10;

/// A community proposal to flag a file hash as a threat.
#[derive(CandidType, Deserialize, Clone)]
pub struct Proposal {
    /// Auto-assigned sequential id.
    pub id: u64,
    /// Hex-encoded SHA-256 hash under review.
    pub sha256: String,
    /// device_id of the submitter.
    pub submitter: String,
    /// Number of approve votes cast so far.
    pub approve_votes: u64,
    /// Number of reject votes cast so far.
    pub reject_votes: u64,
    /// Current status: "pending" | "approved" | "rejected".
    pub status: String,
    /// Unix nanoseconds of submission time.
    pub created_at: u64,
}

impl Storable for Proposal {
    /// Serialises the Proposal to Candid bytes for stable memory storage.
    fn to_bytes(&self) -> Cow<[u8]> {
        Cow::Owned(Encode!(self).expect("Proposal encode failed"))
    }

    /// Deserialises a Proposal from Candid bytes.
    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        Decode!(bytes.as_ref(), Self).expect("Proposal decode failed")
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 512,
        is_fixed_size: false,
    };
}

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

    /// All proposals keyed by id.
    static PROPOSALS: RefCell<StableBTreeMap<u64, Proposal, Memory>> = RefCell::new(
        StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(0)))
        )
    );

    /// Reputation scores keyed by device_id.
    static REPUTATION: RefCell<StableBTreeMap<String, u64, Memory>> = RefCell::new(
        StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(1)))
        )
    );

    /// Monotonic proposal id counter. Stored at key 0u8.
    static NEXT_ID: RefCell<StableBTreeMap<u8, u64, Memory>> = RefCell::new(
        StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(2)))
        )
    );

    /// Vote deduplication. Key = "{proposal_id}:{voter_id}", value = true.
    static VOTES: RefCell<StableBTreeMap<String, bool, Memory>> = RefCell::new(
        StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(3)))
        )
    );
}

/// Increments and returns the next proposal id. IDs start at 1 — 0 is reserved
/// as a sentinel for "not set" in client code.
fn next_proposal_id() -> u64 {
    NEXT_ID.with(|m| {
        let mut map = m.borrow_mut();
        let next = map.get(&0u8).unwrap_or(0) + 1;
        map.insert(0u8, next);
        next
    })
}

/// Increments the reputation of a device by 1.
///
/// Args:
///   device_id: The device whose reputation to increment.
fn increment_reputation(device_id: &str) {
    REPUTATION.with(|m| {
        let mut map = m.borrow_mut();
        let current = map.get(&device_id.to_string()).unwrap_or(0);
        map.insert(device_id.to_string(), current + 1);
    });
}

/// Submits a hash for community review and returns the new proposal id.
/// Submitter identity is derived from ic_cdk::caller() — cannot be spoofed.
/// Rewards the submitter with +1 reputation.
///
/// Args:
///   sha256: Hex-encoded SHA-256 hash of the file to review.
///
/// Returns:
///   nat64 — id of the newly created proposal.
#[ic_cdk::update]
fn submit_proposal(sha256: String) -> u64 {
    let submitter = ic_cdk::caller().to_text();
    let id = next_proposal_id();
    let proposal = Proposal {
        id,
        sha256,
        submitter: submitter.clone(),
        approve_votes: 0,
        reject_votes: 0,
        status: "pending".to_string(),
        created_at: ic_cdk::api::time(),
    };
    PROPOSALS.with(|m| m.borrow_mut().insert(id, proposal));
    increment_reputation(&submitter);
    id
}

/// Casts a vote on a pending proposal.
/// Voter identity is derived from ic_cdk::caller() — cannot be spoofed.
/// Returns Err if the caller has already voted on this proposal so the caller
/// can distinguish a successful vote from a no-op.
/// Auto-resolves the proposal when approve or reject votes reach VOTE_THRESHOLD.
/// Rewards the voter with +1 reputation.
///
/// Args:
///   proposal_id: Id of the proposal to vote on.
///   approve:     true to approve, false to reject.
///
/// Returns:
///   Ok(()) on success, Err(message) if the caller already voted on this proposal.
#[ic_cdk::update]
fn vote(proposal_id: u64, approve: bool) -> Result<(), String> {
    let voter = ic_cdk::caller().to_text();
    let dedup_key = format!("{proposal_id}:{voter}");

    if VOTES.with(|m| m.borrow().get(&dedup_key).is_some()) {
        return Err(format!("already voted on proposal {proposal_id}"));
    }

    VOTES.with(|m| m.borrow_mut().insert(dedup_key, true));

    PROPOSALS.with(|m| {
        let mut map = m.borrow_mut();
        if let Some(mut proposal) = map.get(&proposal_id) {
            if proposal.status != "pending" {
                return;
            }
            if approve {
                proposal.approve_votes += 1;
                if proposal.approve_votes >= VOTE_THRESHOLD {
                    proposal.status = "approved".to_string();
                }
            } else {
                proposal.reject_votes += 1;
                if proposal.reject_votes >= VOTE_THRESHOLD {
                    proposal.status = "rejected".to_string();
                }
            }
            map.insert(proposal_id, proposal);
        }
    });

    increment_reputation(&voter);
    Ok(())
}

/// Returns all proposals in the order they were submitted.
///
/// Returns:
///   vec Proposal — all proposals, sorted by id ascending.
#[ic_cdk::query]
fn get_proposals() -> Vec<Proposal> {
    PROPOSALS.with(|m| m.borrow().iter().map(|(_, v)| v).collect())
}

/// Returns the reputation score for a device.
/// Returns 0 for unknown devices.
///
/// Args:
///   device_id: The device whose reputation to look up.
///
/// Returns:
///   nat64 — accumulated reputation points.
#[ic_cdk::query]
fn get_reputation(device_id: String) -> u64 {
    REPUTATION.with(|m| m.borrow().get(&device_id).unwrap_or(0))
}

ic_cdk::export_candid!();
