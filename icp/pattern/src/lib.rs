use candid::{CandidType, Decode, Encode};
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::storable::Bound;
use ic_stable_structures::{DefaultMemoryImpl, StableVec, Storable};
use serde::Deserialize;
use std::borrow::Cow;
use std::cell::RefCell;

type Memory = VirtualMemory<DefaultMemoryImpl>;

/// A single entry in the pattern ledger.
/// Stored in stable memory via Candid encoding so upgrades preserve all data.
#[derive(CandidType, Deserialize, Clone)]
pub struct PatternEntry {
    /// Sequential id equal to the entry's index in the ledger.
    pub id: u64,
    /// Hex-encoded SHA-256 hash of the file.
    pub sha256: String,
    /// Threat severity: "low" | "medium" | "high" | "critical".
    pub threat_level: String,
    /// Submitter device_id or "community".
    pub source: String,
    /// Unix nanoseconds from ic_cdk::api::time().
    pub timestamp: u64,
}

impl Storable for PatternEntry {
    /// Serialises the entry to Candid bytes for stable memory storage.
    fn to_bytes(&self) -> Cow<[u8]> {
        Cow::Owned(Encode!(self).expect("PatternEntry encode failed"))
    }

    /// Deserialises a PatternEntry from Candid bytes.
    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        Decode!(bytes.as_ref(), Self).expect("PatternEntry decode failed")
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 512,
        is_fixed_size: false,
    };
}

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

    /// Append-only ledger. Entry index == entry id.
    static LEDGER: RefCell<StableVec<PatternEntry, Memory>> = RefCell::new(
        StableVec::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(0)))
        ).expect("LEDGER StableVec init failed")
    );
}

/// Returns the id of the most recently appended pattern entry.
/// Returns 0 if the ledger is empty (safe default for delta sync seed).
///
/// Returns:
///   nat64 — id of the last entry, or 0 if the ledger has no entries.
#[ic_cdk::query]
fn get_latest_id() -> u64 {
    LEDGER.with(|v| v.borrow().len().saturating_sub(1))
}

/// Returns the SHA-256 hash of the most recently appended entry.
/// Returns an empty string if the ledger has no entries.
///
/// Returns:
///   text — SHA-256 hex string of the latest entry, or "" if empty.
#[ic_cdk::query]
fn get_latest_hash() -> String {
    LEDGER.with(|v| {
        let ledger = v.borrow();
        let len = ledger.len();
        if len == 0 {
            return String::new();
        }
        ledger.get(len - 1).map(|e| e.sha256).unwrap_or_default()
    })
}

/// Returns all entries appended after `since_id` (exclusive).
/// Used by the Phase 11 twelve-hour sync loop to fetch only new entries.
///
/// Args:
///   since_id: The last id the caller already has. Pass 0 on first sync.
///
/// Returns:
///   vec PatternEntry — entries with id > since_id, in ascending order.
#[ic_cdk::query]
fn get_delta_since(since_id: u64) -> Vec<PatternEntry> {
    LEDGER.with(|v| {
        let ledger = v.borrow();
        let len = ledger.len();
        let start = since_id.saturating_add(1);
        if start >= len {
            return vec![];
        }
        (start..len)
            .filter_map(|i| ledger.get(i))
            .collect()
    })
}

/// Appends a new pattern entry to the ledger and returns its assigned id.
/// NOTE: no caller check in Phase 10 — Phase 13 will add an API key gate.
///
/// Args:
///   sha256:       Hex-encoded SHA-256 hash of the flagged file.
///   threat_level: Severity string ("low" | "medium" | "high" | "critical").
///   source:       Submitter identifier or "community".
///
/// Returns:
///   nat64 — the id assigned to the new entry.
#[ic_cdk::update]
fn submit_pattern(sha256: String, threat_level: String, source: String) -> u64 {
    LEDGER.with(|v| {
        let ledger = v.borrow_mut();
        let id = ledger.len();
        let entry = PatternEntry {
            id,
            sha256,
            threat_level,
            source,
            timestamp: ic_cdk::api::time(),
        };
        ledger.push(&entry).expect("LEDGER push failed");
        id
    })
}

ic_cdk::export_candid!();
