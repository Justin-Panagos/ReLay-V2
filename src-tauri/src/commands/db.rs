use crate::db::{self, DbState};
use tauri::State;

/// Returns the value of a setting by key, or null if the key does not exist.
///
/// Args:
///   key:   The setting key to retrieve.
///   state: Tauri-managed database state.
///
/// Returns:
///   Ok(Some(value)) if found, Ok(None) if not found, Err(message) on failure.
#[tauri::command]
pub fn get_setting(
    key: String,
    state: State<DbState>,
) -> Result<Option<String>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::get_setting(&conn, &key).map_err(|e| e.to_string())
}

/// Inserts or updates a setting value.
///
/// Args:
///   key:   The setting key.
///   value: The value to store.
///   state: Tauri-managed database state.
///
/// Returns:
///   Ok(()) on success, Err(message) on failure.
#[tauri::command]
pub fn set_setting(
    key: String,
    value: String,
    state: State<DbState>,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, &key, &value).map_err(|e| e.to_string())
}
