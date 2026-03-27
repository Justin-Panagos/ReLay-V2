use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::storable::Bound;
use ic_stable_structures::{DefaultMemoryImpl, StableBTreeMap, Storable};
use std::borrow::Cow;
use std::cell::RefCell;

type Memory = VirtualMemory<DefaultMemoryImpl>;

/// A bounded UTF-8 string suitable for use as a StableBTreeMap value.
/// Plain `String` uses Bound::Unbounded and cannot be stored in a StableBTreeMap.
#[derive(Clone)]
struct BoundedString(pub String);

impl Storable for BoundedString {
    /// Serialises the string as raw UTF-8 bytes.
    fn to_bytes(&self) -> Cow<[u8]> {
        Cow::Borrowed(self.0.as_bytes())
    }

    /// Deserialises a BoundedString from raw UTF-8 bytes.
    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        BoundedString(String::from_utf8(bytes.into_owned()).expect("BoundedString: invalid UTF-8"))
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 4096,
        is_fixed_size: false,
    };
}

/// Storage keys for the META map.
const KEY_VERSION: u8 = 0;
const KEY_CHANGELOG: u8 = 1;

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

    /// Two-entry map: key 0 = version string, key 1 = changelog text.
    static META: RefCell<StableBTreeMap<u8, BoundedString, Memory>> = RefCell::new(
        StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(0)))
        )
    );
}

/// Returns the latest published app version string (e.g. "0.1.0").
/// Returns an empty string if no version has been set yet.
///
/// Returns:
///   text — semver version string, or "" if unset.
#[ic_cdk::query]
fn get_latest_version() -> String {
    META.with(|m| {
        m.borrow()
            .get(&KEY_VERSION)
            .map(|s| s.0)
            .unwrap_or_default()
    })
}

/// Returns the changelog text for the latest version.
/// Returns an empty string if no version has been set yet.
///
/// Returns:
///   text — changelog markdown or plain text, or "" if unset.
#[ic_cdk::query]
fn get_changelog() -> String {
    META.with(|m| {
        m.borrow()
            .get(&KEY_CHANGELOG)
            .map(|s| s.0)
            .unwrap_or_default()
    })
}

/// Publishes a new app version and its changelog.
///
/// Args:
///   version:   Semver string (e.g. "0.2.0").
///   changelog: Release notes or change description.
#[ic_cdk::update]
fn set_version(version: String, changelog: String) {
    META.with(|m| {
        let mut map = m.borrow_mut();
        map.insert(KEY_VERSION, BoundedString(version));
        map.insert(KEY_CHANGELOG, BoundedString(changelog));
    });
}

ic_cdk::export_candid!();
