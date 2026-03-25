use rusqlite::{Connection, Result};
use rusqlite_migration::{Migrations, M};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;

/// Shared database state managed by Tauri.
/// Wrapped in Mutex so Tauri commands can safely access it across threads.
pub struct DbState(pub Mutex<Connection>);

/// All schema migrations in chronological order.
/// Never edit existing entries — only append new ones.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        // v1 — initial schema
        M::up(
            "CREATE TABLE settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE downloads (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                url              TEXT NOT NULL,
                filename         TEXT NOT NULL,
                destination      TEXT NOT NULL,
                status           TEXT NOT NULL,
                size_bytes       INTEGER,
                downloaded_bytes INTEGER NOT NULL DEFAULT 0,
                created_at       TEXT NOT NULL,
                completed_at     TEXT
            );",
        ),
        // v2 — pause/resume support
        M::up(
            "ALTER TABLE downloads ADD COLUMN paused_at TEXT;
            CREATE TABLE chunk_offsets (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                download_id   INTEGER NOT NULL,
                chunk_idx     INTEGER NOT NULL,
                start_byte    INTEGER NOT NULL,
                end_byte      INTEGER NOT NULL,
                written_bytes INTEGER NOT NULL DEFAULT 0
            );",
        ),
    ])
}

/// Initialises the SQLite database at `{app_data_dir}/relay.db`.
/// Creates the directory if it does not exist, runs all pending migrations,
/// and seeds default settings on first launch.
///
/// Args:
///   app_data_dir: Path to the OS-specific app data directory from Tauri's path resolver.
///
/// Returns:
///   An open rusqlite Connection ready for use.
pub fn init_db(app_data_dir: PathBuf) -> Result<Connection> {
    std::fs::create_dir_all(&app_data_dir)
        .expect("could not create app data directory");

    let db_path = app_data_dir.join("relay.db");
    let mut conn = Connection::open(db_path)?;

    migrations()
        .to_latest(&mut conn)
        .expect("database migration failed");

    seed_defaults(&conn);

    Ok(conn)
}

/// Inserts default settings if the settings table is empty (i.e. first launch).
///
/// Args:
///   conn: Open database connection.
fn seed_defaults(conn: &Connection) {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
        .unwrap_or(0);

    if count > 0 {
        return;
    }

    let default_folder = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .to_string_lossy()
        .to_string();

    let defaults = [
        ("default_folder", default_folder.as_str()),
        ("language", "en"),
        ("theme", "auto"),
    ];

    for (key, value) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
            [key, value],
        )
        .ok();
    }
}

/// Retrieves a setting value by key.
///
/// Args:
///   conn: Open database connection.
///   key:  The setting key to look up.
///
/// Returns:
///   Some(value) if the key exists, None otherwise.
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

/// Inserts or updates a setting value.
///
/// Args:
///   conn:  Open database connection.
///   key:   The setting key.
///   value: The value to store.
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// A single row from the downloads table, serialisable for Tauri commands.
#[derive(Debug, Serialize)]
pub struct DownloadRecord {
    pub id: i64,
    pub url: String,
    pub filename: String,
    pub destination: String,
    pub status: String,
    pub size_bytes: Option<i64>,
    pub downloaded_bytes: i64,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub paused_at: Option<String>,
}

/// Per-chunk byte-offset snapshot used to resume a paused chunked download.
#[derive(Debug, Clone)]
pub struct ChunkSnapshot {
    pub chunk_idx: usize,
    pub start_byte: u64,
    pub end_byte: u64,
    pub written_bytes: u64,
}

/// Inserts a new download row with status `queued` and returns its id.
///
/// Args:
///   conn:        Open database connection.
///   url:         The download URL.
///   filename:    The target filename.
///   destination: The destination directory path.
///
/// Returns:
///   The auto-assigned row id of the new download.
pub fn insert_download(
    conn: &Connection,
    url: &str,
    filename: &str,
    destination: &str,
) -> Result<i64> {
    let now = unix_now();
    conn.execute(
        "INSERT INTO downloads (url, filename, destination, status, created_at)
         VALUES (?1, ?2, ?3, 'queued', ?4)",
        rusqlite::params![url, filename, destination, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Updates the status field of a download row.
/// Also sets `completed_at` when status is "complete", and `paused_at` when status is "paused".
///
/// Args:
///   conn:   Open database connection.
///   id:     The download row id.
///   status: New status string (queued|downloading|paused|complete|failed|quarantined).
pub fn update_download_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    if status == "complete" {
        conn.execute(
            "UPDATE downloads SET status = ?1, completed_at = ?2 WHERE id = ?3",
            rusqlite::params![status, unix_now(), id],
        )?;
    } else if status == "paused" {
        conn.execute(
            "UPDATE downloads SET status = ?1, paused_at = ?2 WHERE id = ?3",
            rusqlite::params![status, unix_now(), id],
        )?;
    } else {
        conn.execute(
            "UPDATE downloads SET status = ?1 WHERE id = ?2",
            rusqlite::params![status, id],
        )?;
    }
    Ok(())
}

/// Resets all in-progress downloads to `failed`.
/// Called on app startup to clean up downloads interrupted by a previous crash or close.
///
/// Args:
///   conn: Open database connection.
pub fn reset_stale_downloads(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET status = 'failed' WHERE status IN ('downloading', 'queued')",
        [],
    )?;
    Ok(())
}

/// Sets the size_bytes field for a download row.
/// Called after a successful HEAD request reveals Content-Length.
///
/// Args:
///   conn:       Active SQLite connection.
///   id:         The download row id.
///   size_bytes: Total file size in bytes.
///
/// Returns:
///   Ok(()) on success.
pub fn update_download_size(conn: &Connection, id: i64, size_bytes: u64) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET size_bytes = ?1 WHERE id = ?2",
        rusqlite::params![size_bytes as i64, id],
    )?;
    Ok(())
}

/// Returns the most recent download rows ordered by created_at descending.
///
/// Args:
///   conn:  Open database connection.
///   limit: Maximum number of rows to return.
///
/// Returns:
///   Vec of DownloadRecord structs ordered newest-first.
pub fn get_recent_downloads(conn: &Connection, limit: i64) -> Result<Vec<DownloadRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, url, filename, destination, status,
                size_bytes, downloaded_bytes, created_at, completed_at, paused_at
         FROM downloads
         ORDER BY created_at DESC
         LIMIT ?1",
    )?;

    let rows = stmt.query_map([limit], |row| {
        Ok(DownloadRecord {
            id: row.get(0)?,
            url: row.get(1)?,
            filename: row.get(2)?,
            destination: row.get(3)?,
            status: row.get(4)?,
            size_bytes: row.get(5)?,
            downloaded_bytes: row.get(6)?,
            created_at: row.get(7)?,
            completed_at: row.get(8)?,
            paused_at: row.get(9)?,
        })
    })?;

    rows.collect()
}

/// Returns a single download row by id.
///
/// Args:
///   conn: Open database connection.
///   id:   The download row id.
///
/// Returns:
///   Some(DownloadRecord) if found, None otherwise.
pub fn get_download_by_id(conn: &Connection, id: i64) -> Result<Option<DownloadRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, url, filename, destination, status,
                size_bytes, downloaded_bytes, created_at, completed_at, paused_at
         FROM downloads WHERE id = ?1",
    )?;

    let mut rows = stmt.query_map([id], |row| {
        Ok(DownloadRecord {
            id: row.get(0)?,
            url: row.get(1)?,
            filename: row.get(2)?,
            destination: row.get(3)?,
            status: row.get(4)?,
            size_bytes: row.get(5)?,
            downloaded_bytes: row.get(6)?,
            created_at: row.get(7)?,
            completed_at: row.get(8)?,
            paused_at: row.get(9)?,
        })
    })?;

    match rows.next() {
        Some(record) => Ok(Some(record?)),
        None => Ok(None),
    }
}

/// Persists per-chunk byte-offset snapshots for a paused download.
/// Deletes any existing snapshots for the download before inserting new ones.
///
/// Args:
///   conn:        Open database connection.
///   download_id: The download row id.
///   snapshots:   Slice of ChunkSnapshot structs to persist.
pub fn save_chunk_snapshots(
    conn: &Connection,
    download_id: i64,
    snapshots: &[ChunkSnapshot],
) -> Result<()> {
    conn.execute(
        "DELETE FROM chunk_offsets WHERE download_id = ?1",
        [download_id],
    )?;
    for snap in snapshots {
        conn.execute(
            "INSERT INTO chunk_offsets (download_id, chunk_idx, start_byte, end_byte, written_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                download_id,
                snap.chunk_idx as i64,
                snap.start_byte as i64,
                snap.end_byte as i64,
                snap.written_bytes as i64
            ],
        )?;
    }
    Ok(())
}

/// Loads per-chunk byte-offset snapshots for a paused download, ordered by chunk_idx.
///
/// Args:
///   conn:        Open database connection.
///   download_id: The download row id.
///
/// Returns:
///   Vec of ChunkSnapshot structs ordered by chunk_idx ascending.
pub fn load_chunk_snapshots(conn: &Connection, download_id: i64) -> Result<Vec<ChunkSnapshot>> {
    let mut stmt = conn.prepare(
        "SELECT chunk_idx, start_byte, end_byte, written_bytes
         FROM chunk_offsets
         WHERE download_id = ?1
         ORDER BY chunk_idx ASC",
    )?;

    let rows = stmt.query_map([download_id], |row| {
        Ok(ChunkSnapshot {
            chunk_idx: row.get::<_, i64>(0)? as usize,
            start_byte: row.get::<_, i64>(1)? as u64,
            end_byte: row.get::<_, i64>(2)? as u64,
            written_bytes: row.get::<_, i64>(3)? as u64,
        })
    })?;

    rows.collect()
}

/// Deletes all chunk_offsets rows for a download.
/// Called after a download completes, is cancelled, or resumes successfully.
///
/// Args:
///   conn:        Open database connection.
///   download_id: The download row id.
pub fn delete_chunk_snapshots(conn: &Connection, download_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM chunk_offsets WHERE download_id = ?1",
        [download_id],
    )?;
    Ok(())
}

/// Returns the current time as a Unix timestamp string.
fn unix_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}
