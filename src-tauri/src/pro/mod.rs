use std::sync::Mutex;
use std::time::Instant;

/// In-memory licence state populated exclusively by ICP canister responses.
/// SQLite is used only to seed this cache at startup and to persist it for the
/// next restart — it is never the source of truth for gate checks.
pub struct LicenceCache {
    /// "pro" or "free".
    pub status: String,
    /// Unix seconds when the licence expires; 0 means no stored expiry.
    pub expiry: u64,
    /// Monotonic timestamp of the last successful ICP verification.
    pub verified_at: Instant,
}

/// Tauri-managed wrapper around the in-memory licence cache.
pub struct LicenceCacheState(pub Mutex<LicenceCache>);

/// Overwrites the in-memory licence cache with the result of an ICP verification.
/// This is the only intended write path — no other code should mutate `LicenceCache`.
///
/// Args:
///   cache:  The managed `LicenceCacheState`.
///   status: "pro" or "free".
///   expiry: Unix seconds of licence expiry, or 0 for free.
pub fn update_licence_cache(cache: &LicenceCacheState, status: &str, expiry: u64) {
    let mut lock = cache.0.lock().unwrap_or_else(|p| p.into_inner());
    lock.status = status.to_string();
    lock.expiry = expiry;
    lock.verified_at = Instant::now();
}

/// Returns true if the in-memory licence cache indicates an active Pro subscription.
/// Checks that the status is "pro" and, if an expiry timestamp is set, that it has
/// not yet passed.
///
/// Args:
///   cache: The managed `LicenceCacheState`.
///
/// Returns:
///   true if the cached licence is currently active Pro.
pub fn is_pro(cache: &LicenceCacheState) -> bool {
    let lock = cache.0.lock().unwrap_or_else(|p| p.into_inner());
    if lock.status != "pro" {
        return false;
    }
    if lock.expiry == 0 {
        return true;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    lock.expiry > now
}

/// Returns Ok(()) if the in-memory licence cache indicates active Pro, Err otherwise.
/// Intended call site for all Pro feature gates in commands/ (CLAUDE.md convention).
/// Currently commands call is_pro() directly; this function is preserved so future
/// Pro-gated commands have a single consistent entry point.
///
/// Args:
///   cache: The managed `LicenceCacheState`.
///
/// Returns:
///   Ok(()) if Pro, Err with a user-facing message if not.
#[allow(dead_code)]
pub fn require_pro(cache: &LicenceCacheState) -> Result<(), String> {
    if is_pro(cache) {
        Ok(())
    } else {
        Err("Pro subscription required".into())
    }
}
