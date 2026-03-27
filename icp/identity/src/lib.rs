use candid::CandidType;
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::{DefaultMemoryImpl, StableBTreeMap};
use serde::Deserialize;
use std::cell::RefCell;

type Memory = VirtualMemory<DefaultMemoryImpl>;

/// Licence status returned by check_licence.
/// Free: no active subscription.
/// Pro:  active subscription with an expiry timestamp (Unix seconds).
#[derive(CandidType, Deserialize, Clone)]
pub enum LicenceStatus {
    Free,
    Pro { expiry_timestamp: u64 },
}

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

    /// Maps device_id (String) to expiry_timestamp (u64).
    /// Encoding: 0 = Free, any nonzero value = Pro expiry (Unix seconds).
    static LICENCES: RefCell<StableBTreeMap<String, u64, Memory>> = RefCell::new(
        StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(0)))
        )
    );
}

/// Returns the licence status for the given device id.
/// A missing or zero-valued entry is treated as Free.
///
/// Args:
///   device_id: The device's unique identifier string.
///
/// Returns:
///   LicenceStatus — Free or Pro { expiry_timestamp }.
#[ic_cdk::query]
fn check_licence(device_id: String) -> LicenceStatus {
    LICENCES.with(|m| {
        match m.borrow().get(&device_id) {
            None | Some(0) => LicenceStatus::Free,
            Some(ts) => LicenceStatus::Pro { expiry_timestamp: ts },
        }
    })
}

/// Registers a device on the Free tier.
/// Does nothing if the device_id is already registered.
///
/// Args:
///   device_id: The device's unique identifier string.
#[ic_cdk::update]
fn register_device(device_id: String) {
    LICENCES.with(|m| {
        let mut map = m.borrow_mut();
        if map.get(&device_id).is_none() {
            map.insert(device_id, 0u64);
        }
    });
}

/// Grants a Pro licence to the specified device until expiry_timestamp.
/// FIXME(Phase-12): add controller-only caller check before mainnet deployment.
///
/// Args:
///   device_id:        The device's unique identifier string.
///   expiry_timestamp: Unix seconds at which the Pro licence expires.
#[ic_cdk::update]
fn grant_pro(device_id: String, expiry_timestamp: u64) {
    LICENCES.with(|m| {
        m.borrow_mut().insert(device_id, expiry_timestamp);
    });
}

ic_cdk::export_candid!();
