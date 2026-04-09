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

    /// Maps API key (String) to expiry_timestamp (u64).
    /// Encoding: 0 = revoked, any nonzero value = active until that Unix second.
    /// MemoryId(1) — must never change after first deployment.
    static API_KEYS: RefCell<StableBTreeMap<String, u64, Memory>> = RefCell::new(
        StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(1)))
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

/// Grants or revokes a Pro licence for the specified device.
/// Callable only by a canister controller (the Cloudflare Worker's ICP identity).
/// To revoke, pass expiry_timestamp = 0.
///
/// Args:
///   device_id:        The device's unique identifier string.
///   expiry_timestamp: Unix seconds at which Pro expires; 0 = Free.
#[ic_cdk::update]
fn grant_pro(device_id: String, expiry_timestamp: u64) {
    assert!(
        ic_cdk::api::is_controller(&ic_cdk::caller()),
        "only a canister controller may call grant_pro"
    );
    LICENCES.with(|m| {
        m.borrow_mut().insert(device_id, expiry_timestamp);
    });
}

/// Grants or extends a Threat Intelligence API key.
/// Callable only by a canister controller (the Cloudflare Worker's ICP identity).
/// To revoke, pass expiry_timestamp = 0 or call revoke_api_key instead.
///
/// Args:
///   api_key:          The 64-character hex API key string.
///   expiry_timestamp: Unix seconds at which the key expires; 0 = revoked.
#[ic_cdk::update]
fn grant_api_key(api_key: String, expiry_timestamp: u64) {
    assert!(
        ic_cdk::api::is_controller(&ic_cdk::caller()),
        "only a canister controller may call grant_api_key"
    );
    API_KEYS.with(|m| {
        m.borrow_mut().insert(api_key, expiry_timestamp);
    });
}

/// Returns true if the API key exists and has not expired.
/// Converts ic_cdk::api::time() nanoseconds to Unix seconds for comparison.
///
/// Args:
///   api_key: The 64-character hex API key string to validate.
///
/// Returns:
///   bool — true if the key is active and not expired, false otherwise.
#[ic_cdk::query]
fn validate_api_key(api_key: String) -> bool {
    API_KEYS.with(|m| {
        match m.borrow().get(&api_key) {
            None | Some(0) => false,
            Some(expiry) => {
                let now_secs = ic_cdk::api::time() / 1_000_000_000;
                expiry > now_secs
            }
        }
    })
}

/// Revokes an API key immediately by setting its expiry to 0.
/// Callable only by a canister controller.
///
/// Args:
///   api_key: The 64-character hex API key string to revoke.
#[ic_cdk::update]
fn revoke_api_key(api_key: String) {
    assert!(
        ic_cdk::api::is_controller(&ic_cdk::caller()),
        "only a canister controller may call revoke_api_key"
    );
    API_KEYS.with(|m| {
        m.borrow_mut().insert(api_key, 0u64);
    });
}

ic_cdk::export_candid!();
