use crate::db;
use rusqlite::Connection;

/// Returns true if the current licence status is "pro".
/// Phase 12 will replace this with real payment verification.
///
/// Args:
///   conn: Open database connection.
///
/// Returns:
///   true if licence_status setting equals "pro", false otherwise.
pub fn is_pro(conn: &Connection) -> bool {
    db::get_setting(conn, "licence_status")
        .ok()
        .flatten()
        .as_deref()
        == Some("pro")
}

/// Returns Ok(()) if the current licence is Pro, Err otherwise.
///
/// Args:
///   conn: Open database connection.
///
/// Returns:
///   Ok(()) if Pro, Err with a user-facing message if not.
#[allow(dead_code)] // used in Phase 13 for API access gating
pub fn require_pro(conn: &Connection) -> Result<(), String> {
    if is_pro(conn) {
        Ok(())
    } else {
        Err("Pro subscription required".into())
    }
}
