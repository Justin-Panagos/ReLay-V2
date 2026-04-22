use crate::db::{self, DbState, QuarantineRecord};
use tauri::State;

/// Returns recent quarantine entries ordered by quarantined_at descending.
///
/// Args:
///   limit: Maximum number of rows to return.
///   db:    Tauri-managed database state.
///
/// Returns:
///   Vec of QuarantineRecord on success, or an error string.
#[tauri::command]
pub fn get_quarantine(
    limit: i64,
    db: State<'_, DbState>,
) -> Result<Vec<QuarantineRecord>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::get_quarantine(&conn, limit).map_err(|e| e.to_string())
}

/// Restores a quarantined file to its original location.
/// Moves the file from quarantine_path back to original_path,
/// updates the download status to "complete", and deletes the quarantine row.
///
/// Args:
///   id: The quarantine table row id.
///   db: Tauri-managed database state.
///
/// Returns:
///   Ok(()) on success, Err with message on failure.
#[tauri::command]
pub fn restore_quarantine(id: i64, db: State<'_, DbState>) -> Result<(), String> {
    let record = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_quarantine_by_id(&conn, id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("quarantine entry {id} not found"))?
    };

    // Move file back to original location.
    if let Some(parent) = std::path::Path::new(&record.original_path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&record.quarantine_path, &record.original_path)
        .map_err(|e| e.to_string())?;

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_download_status(&conn, record.download_id, "complete")
        .map_err(|e| e.to_string())?;
    db::delete_quarantine_entry(&conn, id).map_err(|e| e.to_string())?;

    Ok(())
}

/// Permanently deletes a quarantined file and removes its quarantine record.
///
/// Args:
///   id: The quarantine table row id.
///   db: Tauri-managed database state.
///
/// Returns:
///   Ok(()) on success, Err with message on failure.
#[tauri::command]
pub fn delete_quarantine(id: i64, db: State<'_, DbState>) -> Result<(), String> {
    let record = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_quarantine_by_id(&conn, id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("quarantine entry {id} not found"))?
    };

    // Delete the file (ignore error if already gone).
    std::fs::remove_file(&record.quarantine_path).ok();

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_download_status(&conn, record.download_id, "failed")
        .map_err(|e| e.to_string())?;
    db::delete_quarantine_entry(&conn, id).map_err(|e| e.to_string())?;

    Ok(())
}
