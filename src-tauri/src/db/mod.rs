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
        // v3 — torrent support
        M::up(
            "ALTER TABLE downloads ADD COLUMN type TEXT NOT NULL DEFAULT 'http';
            ALTER TABLE downloads ADD COLUMN torrent_id INTEGER;",
        ),
        // v4 — shield scanning results and quarantine
        M::up(
            "ALTER TABLE downloads ADD COLUMN sha256 TEXT;
            ALTER TABLE downloads ADD COLUMN scan_threat TEXT;
            CREATE TABLE quarantine (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                download_id      INTEGER NOT NULL,
                sha256           TEXT NOT NULL,
                filename         TEXT NOT NULL,
                original_path    TEXT NOT NULL,
                quarantine_path  TEXT NOT NULL,
                threat_reason    TEXT NOT NULL,
                quarantined_at   TEXT NOT NULL
            );",
        ),
        // v5 — sandbox scan report (Layer 7, Pro)
        M::up("ALTER TABLE downloads ADD COLUMN sandbox_report TEXT;"),
        // v6 — ICP pattern cache for offline Shield lookups
        M::up(
            "CREATE TABLE icp_patterns (
                id           INTEGER PRIMARY KEY,
                sha256       TEXT NOT NULL UNIQUE,
                threat_level TEXT NOT NULL,
                source       TEXT NOT NULL,
                timestamp    INTEGER NOT NULL
            );
            CREATE INDEX icp_patterns_sha256_idx ON icp_patterns (sha256);",
        ),
        // v7 — auto-retry counter + bandwidth limit + schedule window (Pro queue management)
        M::up(
            "ALTER TABLE downloads ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE downloads ADD COLUMN bandwidth_limit_kbps INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE downloads ADD COLUMN schedule_start TEXT;
             ALTER TABLE downloads ADD COLUMN schedule_end TEXT;",
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
///   An open rusqlite Connection ready for use, or an error message string.
pub fn init_db(app_data_dir: PathBuf) -> std::result::Result<Connection, String> {
    std::fs::create_dir_all(&app_data_dir)
        .map_err(|e| format!("could not create app data directory: {e}"))?;

    let db_path = app_data_dir.join("relay.db");
    let mut conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    migrations()
        .to_latest(&mut conn)
        .map_err(|e| format!("database migration failed: {e}"))?;

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
    pub type_: String,
    pub torrent_id: Option<i64>,
    pub sha256: Option<String>,
    pub scan_threat: Option<String>,
    pub sandbox_report: Option<String>,
    pub retry_count: i64,
    pub schedule_start: Option<String>,
    pub schedule_end: Option<String>,
    pub bandwidth_limit_kbps: i64,
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
                size_bytes, downloaded_bytes, created_at, completed_at, paused_at,
                type, torrent_id, sha256, scan_threat, sandbox_report,
                retry_count, schedule_start, schedule_end, bandwidth_limit_kbps
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
            type_: row.get(10)?,
            torrent_id: row.get(11)?,
            sha256: row.get(12)?,
            scan_threat: row.get(13)?,
            sandbox_report: row.get(14)?,
            retry_count: row.get(15)?,
            schedule_start: row.get(16)?,
            schedule_end: row.get(17)?,
            bandwidth_limit_kbps: row.get(18)?,
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
                size_bytes, downloaded_bytes, created_at, completed_at, paused_at,
                type, torrent_id, sha256, scan_threat, sandbox_report,
                retry_count, schedule_start, schedule_end, bandwidth_limit_kbps
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
            type_: row.get(10)?,
            torrent_id: row.get(11)?,
            sha256: row.get(12)?,
            scan_threat: row.get(13)?,
            sandbox_report: row.get(14)?,
            retry_count: row.get(15)?,
            schedule_start: row.get(16)?,
            schedule_end: row.get(17)?,
            bandwidth_limit_kbps: row.get(18)?,
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

/// Inserts a new torrent row with status `queued` and `type = 'torrent'` and returns its id.
///
/// Args:
///   conn:        Open database connection.
///   url:         The magnet link or .torrent file path used to identify the torrent.
///   filename:    Display name (torrent name, may be "Resolving..." initially).
///   destination: The destination directory path.
///
/// Returns:
///   The auto-assigned row id of the new torrent download.
pub fn insert_torrent(
    conn: &Connection,
    url: &str,
    filename: &str,
    destination: &str,
) -> Result<i64> {
    let now = unix_now();
    conn.execute(
        "INSERT INTO downloads (url, filename, destination, status, type, created_at)
         VALUES (?1, ?2, ?3, 'queued', 'torrent', ?4)",
        rusqlite::params![url, filename, destination, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Writes the librqbit-assigned numeric torrent id to a torrent row.
/// Called after the librqbit Session returns a TorrentId for a newly added torrent.
///
/// Args:
///   conn:       Open database connection.
///   id:         The download table row id.
///   torrent_id: The librqbit TorrentId (stored as i64).
pub fn update_torrent_id(conn: &Connection, id: i64, torrent_id: usize) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET torrent_id = ?1 WHERE id = ?2",
        rusqlite::params![torrent_id as i64, id],
    )?;
    Ok(())
}

/// Updates the downloaded_bytes and size_bytes fields for any download row.
/// Called by the torrent progress poller on each tick.
///
/// Args:
///   conn:             Open database connection.
///   id:               The download row id.
///   downloaded_bytes: Bytes downloaded so far.
///   size_bytes:       Total file size in bytes (0 if unknown).
pub fn update_downloaded_bytes(
    conn: &Connection,
    id: i64,
    downloaded_bytes: u64,
    size_bytes: u64,
) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET downloaded_bytes = ?1, size_bytes = ?2 WHERE id = ?3",
        rusqlite::params![downloaded_bytes as i64, size_bytes as i64, id],
    )?;
    Ok(())
}

/// Returns torrent rows (type = 'torrent') ordered by created_at descending.
///
/// Args:
///   conn:  Open database connection.
///   limit: Maximum number of rows to return.
///
/// Returns:
///   Vec of DownloadRecord structs ordered newest-first.
pub fn get_torrents(conn: &Connection, limit: i64) -> Result<Vec<DownloadRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, url, filename, destination, status,
                size_bytes, downloaded_bytes, created_at, completed_at, paused_at,
                type, torrent_id, sha256, scan_threat, sandbox_report,
                retry_count, schedule_start, schedule_end, bandwidth_limit_kbps
         FROM downloads
         WHERE type = 'torrent'
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
            type_: row.get(10)?,
            torrent_id: row.get(11)?,
            sha256: row.get(12)?,
            scan_threat: row.get(13)?,
            sandbox_report: row.get(14)?,
            retry_count: row.get(15)?,
            schedule_start: row.get(16)?,
            schedule_end: row.get(17)?,
            bandwidth_limit_kbps: row.get(18)?,
        })
    })?;

    rows.collect()
}

/// A single row from the quarantine table, serialisable for Tauri commands.
#[derive(Debug, Serialize)]
pub struct QuarantineRecord {
    pub id: i64,
    pub download_id: i64,
    pub sha256: String,
    pub filename: String,
    pub original_path: String,
    pub quarantine_path: String,
    pub threat_reason: String,
    pub quarantined_at: String,
}

/// Inserts a new quarantine row and returns its id.
///
/// Args:
///   conn:           Open database connection.
///   download_id:    The downloads table row id.
///   sha256:         Hex-encoded SHA-256 hash of the file.
///   filename:       The file's name.
///   original_path:  Full path where the file was originally saved.
///   quarantine_path: Full path to the file's current quarantine location.
///   threat_reason:  Human-readable description of why the file was quarantined.
///
/// Returns:
///   The auto-assigned row id of the new quarantine entry.
pub fn insert_quarantine(
    conn: &Connection,
    download_id: i64,
    sha256: &str,
    filename: &str,
    original_path: &str,
    quarantine_path: &str,
    threat_reason: &str,
) -> Result<i64> {
    let now = unix_now();
    conn.execute(
        "INSERT INTO quarantine
            (download_id, sha256, filename, original_path, quarantine_path, threat_reason, quarantined_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![download_id, sha256, filename, original_path, quarantine_path, threat_reason, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Returns quarantine rows ordered by quarantined_at descending.
///
/// Args:
///   conn:  Open database connection.
///   limit: Maximum number of rows to return.
///
/// Returns:
///   Vec of QuarantineRecord structs ordered newest-first.
pub fn get_quarantine(conn: &Connection, limit: i64) -> Result<Vec<QuarantineRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, download_id, sha256, filename, original_path, quarantine_path, threat_reason, quarantined_at
         FROM quarantine
         ORDER BY quarantined_at DESC
         LIMIT ?1",
    )?;

    let rows = stmt.query_map([limit], |row| {
        Ok(QuarantineRecord {
            id: row.get(0)?,
            download_id: row.get(1)?,
            sha256: row.get(2)?,
            filename: row.get(3)?,
            original_path: row.get(4)?,
            quarantine_path: row.get(5)?,
            threat_reason: row.get(6)?,
            quarantined_at: row.get(7)?,
        })
    })?;

    rows.collect()
}

/// Returns a single quarantine row by its id.
///
/// Args:
///   conn: Open database connection.
///   id:   The quarantine table row id.
///
/// Returns:
///   Some(QuarantineRecord) if found, None otherwise.
pub fn get_quarantine_by_id(conn: &Connection, id: i64) -> Result<Option<QuarantineRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, download_id, sha256, filename, original_path, quarantine_path, threat_reason, quarantined_at
         FROM quarantine WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], |row| {
        Ok(QuarantineRecord {
            id: row.get(0)?,
            download_id: row.get(1)?,
            sha256: row.get(2)?,
            filename: row.get(3)?,
            original_path: row.get(4)?,
            quarantine_path: row.get(5)?,
            threat_reason: row.get(6)?,
            quarantined_at: row.get(7)?,
        })
    })?;
    match rows.next() {
        Some(record) => Ok(Some(record?)),
        None => Ok(None),
    }
}

/// Deletes a single quarantine row by its id.
/// Does NOT delete the file on disk — use the command layer for that.
///
/// Args:
///   conn: Open database connection.
///   id:   The quarantine table row id.
pub fn delete_quarantine_entry(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM quarantine WHERE id = ?1", [id])?;
    Ok(())
}

/// Stores the hex-encoded SHA-256 hash for a download row.
///
/// Args:
///   conn:   Open database connection.
///   id:     The download row id.
///   sha256: Hex-encoded SHA-256 digest string.
pub fn update_download_sha256(conn: &Connection, id: i64, sha256: &str) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET sha256 = ?1 WHERE id = ?2",
        rusqlite::params![sha256, id],
    )?;
    Ok(())
}

/// Stores the threat reason string for a quarantined download row.
///
/// Args:
///   conn:   Open database connection.
///   id:     The download row id.
///   reason: Human-readable threat description.
pub fn update_download_scan_threat(conn: &Connection, id: i64, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET scan_threat = ?1 WHERE id = ?2",
        rusqlite::params![reason, id],
    )?;
    Ok(())
}

/// Stores the JSON-encoded sandbox scan report for a download row.
///
/// Args:
///   conn:        Open database connection.
///   id:          The download row id.
///   report_json: JSON string of the SandboxReport struct.
pub fn update_sandbox_report(conn: &Connection, id: i64, report_json: &str) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET sandbox_report = ?1 WHERE id = ?2",
        rusqlite::params![report_json, id],
    )?;
    Ok(())
}

/// Maximum number of ICP pattern rows kept locally. Oldest rows (by timestamp) are
/// pruned after each sync to prevent unbounded SQLite growth if the canister ever
/// ships a very large pattern set.
const MAX_ICP_PATTERN_ROWS: i64 = 100_000;

/// Upserts a batch of ICP pattern entries into the local cache.
/// Uses INSERT OR REPLACE so the same hash can be re-synced with updated metadata.
/// Wraps the batch in a single transaction for performance. After inserting, trims
/// the table to `MAX_ICP_PATTERN_ROWS` by deleting the oldest rows by timestamp.
///
/// Args:
///   conn:    Open database connection.
///   entries: Slice of PatternEntry values received from the ICP pattern canister.
pub fn upsert_icp_patterns(
    conn: &Connection,
    entries: &[crate::icp::agent::PatternEntry],
) -> Result<()> {
    conn.execute_batch("BEGIN")?;
    for entry in entries {
        conn.execute(
            "INSERT OR REPLACE INTO icp_patterns (id, sha256, threat_level, source, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                entry.id as i64,
                entry.sha256,
                entry.threat_level,
                entry.source,
                entry.timestamp as i64,
            ],
        )?;
    }
    // Prune oldest rows to keep the table within the size limit.
    conn.execute(
        "DELETE FROM icp_patterns WHERE id NOT IN (
             SELECT id FROM icp_patterns ORDER BY timestamp DESC LIMIT ?1
         )",
        rusqlite::params![MAX_ICP_PATTERN_ROWS],
    )?;
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// Checks whether a SHA-256 hash exists in the local ICP pattern cache.
///
/// Args:
///   conn:   Open database connection.
///   sha256: Hex-encoded SHA-256 to look up.
///
/// Returns:
///   Some(threat_level) if the hash is known, None if not in the cache.
pub fn lookup_icp_pattern(conn: &Connection, sha256: &str) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT threat_level FROM icp_patterns WHERE sha256 = ?1 LIMIT 1")?;
    let mut rows = stmt.query([sha256])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

/// Returns the total number of threat patterns in the local ICP pattern cache.
///
/// Args:
///   conn: Open database connection.
///
/// Returns:
///   Row count on success, Err on query failure.
pub fn count_icp_patterns(conn: &Connection) -> Result<u64> {
    conn.query_row("SELECT COUNT(*) FROM icp_patterns", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
}

/// Returns true if a non-terminal HTTP download for this URL already exists.
/// Blocks adding a duplicate when the same URL is already queued, downloading, or paused.
///
/// Args:
///   conn: Open database connection.
///   url:  The HTTP URL to check.
///
/// Returns:
///   true if an active/queued/paused download with this URL exists.
pub fn download_exists_for_url(conn: &Connection, url: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM downloads WHERE url = ?1 AND type = 'http'
         AND status IN ('queued','downloading','paused') LIMIT 1",
        rusqlite::params![url],
        |_| Ok(()),
    )
    .is_ok()
}

/// Returns true if a non-terminal torrent for this URL (magnet URI) already exists.
/// Blocks adding a duplicate magnet that is already queued, downloading, or paused.
///
/// Args:
///   conn: Open database connection.
///   url:  The magnet URI or torrent identifier to check.
///
/// Returns:
///   true if an active/queued/paused torrent with this URL exists.
pub fn torrent_exists_for_url(conn: &Connection, url: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM downloads WHERE url = ?1 AND type = 'torrent'
         AND status IN ('queued','downloading','paused') LIMIT 1",
        rusqlite::params![url],
        |_| Ok(()),
    )
    .is_ok()
}

/// Updates the schedule_start and schedule_end columns for a download row.
/// Pass None for either field to clear it (sets the column to NULL).
///
/// Args:
///   conn:  Open database connection.
///   id:    The download row id.
///   start: "HH:MM" 24h window start, or None to clear.
///   end:   "HH:MM" 24h window end, or None to clear.
pub fn set_download_schedule(
    conn: &Connection,
    id: i64,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET schedule_start = ?1, schedule_end = ?2 WHERE id = ?3",
        rusqlite::params![start, end, id],
    )?;
    Ok(())
}

/// Returns all active HTTP downloads (status='active' or 'downloading') that have a
/// schedule window set. Used by the schedule watchdog to decide which downloads to
/// auto-pause when outside their window.
///
/// Args:
///   conn: Open database connection.
///
/// Returns:
///   Vec of (id, schedule_start, schedule_end) tuples.
pub fn get_scheduled_active_downloads(
    conn: &Connection,
) -> Result<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, schedule_start, schedule_end
         FROM downloads
         WHERE status = 'downloading'
           AND schedule_start IS NOT NULL
           AND schedule_end IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    rows.collect()
}

/// Returns all paused HTTP downloads that have a schedule window set.
/// Used by the schedule watchdog to auto-resume downloads inside their window.
///
/// Args:
///   conn: Open database connection.
///
/// Returns:
///   Vec of (id, schedule_start, schedule_end) tuples.
pub fn get_scheduled_paused_downloads(
    conn: &Connection,
) -> Result<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, schedule_start, schedule_end
         FROM downloads
         WHERE status = 'paused'
           AND schedule_start IS NOT NULL
           AND schedule_end IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    rows.collect()
}

/// Increments the retry_count for a download row and returns the new count.
///
/// Args:
///   conn: Open database connection.
///   id:   The download row id.
///
/// Returns:
///   The new retry_count value after incrementing.
pub fn increment_retry_count(conn: &Connection, id: i64) -> Result<u32> {
    conn.execute(
        "UPDATE downloads SET retry_count = retry_count + 1 WHERE id = ?1",
        [id],
    )?;
    let count: u32 = conn.query_row(
        "SELECT retry_count FROM downloads WHERE id = ?1",
        [id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Resets the retry_count for a download row to 0.
/// Called after a successful download completion.
///
/// Args:
///   conn: Open database connection.
///   id:   The download row id.
pub fn reset_retry_count(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET retry_count = 0 WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// Persists a new bandwidth limit for a download row.
/// Pass 0 to remove the limit (unlimited).
///
/// Args:
///   conn: Open database connection.
///   id:   The downloads table row id.
///   kbps: Bandwidth limit in kilobits per second (0 = unlimited).
///
/// Returns:
///   Ok(()) on success.
pub fn update_bandwidth_limit(conn: &Connection, id: i64, kbps: u32) -> Result<()> {
    conn.execute(
        "UPDATE downloads SET bandwidth_limit_kbps = ?1 WHERE id = ?2",
        rusqlite::params![kbps, id],
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icp::agent::PatternEntry;

    /// Opens a fully-migrated in-memory SQLite database for use in tests.
    fn open_test_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn make_entry(id: u64, sha256: &str, threat_level: &str) -> PatternEntry {
        // Constructs a minimal PatternEntry for use in tests.
        PatternEntry {
            id,
            sha256: sha256.to_string(),
            threat_level: threat_level.to_string(),
            source: "test".to_string(),
            timestamp: 1_000_000,
        }
    }

    // ── upsert_icp_patterns / lookup_icp_pattern ─────────────────────────────

    #[test]
    fn upsert_and_lookup_known_hash() {
        // Verifies that a upserted pattern entry can be retrieved by sha256.
        let conn = open_test_db();
        upsert_icp_patterns(&conn, &[make_entry(1, "abc123", "high")]).unwrap();
        let result = lookup_icp_pattern(&conn, "abc123").unwrap();
        assert_eq!(result, Some("high".to_string()));
    }

    #[test]
    fn lookup_unknown_hash_returns_none() {
        // Verifies that looking up a hash that was never inserted returns None.
        let conn = open_test_db();
        let result = lookup_icp_pattern(&conn, "nonexistent").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn upsert_updates_threat_level_on_re_sync() {
        // Verifies that upserting the same id with a different threat_level overwrites the old row.
        let conn = open_test_db();
        upsert_icp_patterns(&conn, &[make_entry(1, "abc123", "low")]).unwrap();
        upsert_icp_patterns(&conn, &[make_entry(1, "abc123", "critical")]).unwrap();
        let result = lookup_icp_pattern(&conn, "abc123").unwrap();
        assert_eq!(result, Some("critical".to_string()));
    }

    #[test]
    fn upsert_batch_inserts_all_entries() {
        // Verifies that a multi-entry batch is fully persisted.
        let conn = open_test_db();
        let entries = vec![
            make_entry(1, "hash1", "low"),
            make_entry(2, "hash2", "medium"),
            make_entry(3, "hash3", "high"),
        ];
        upsert_icp_patterns(&conn, &entries).unwrap();

        assert_eq!(lookup_icp_pattern(&conn, "hash1").unwrap(), Some("low".to_string()));
        assert_eq!(lookup_icp_pattern(&conn, "hash2").unwrap(), Some("medium".to_string()));
        assert_eq!(lookup_icp_pattern(&conn, "hash3").unwrap(), Some("high".to_string()));
    }

    #[test]
    fn upsert_empty_batch_is_noop() {
        // Verifies that upserting an empty slice does not error and leaves the table unchanged.
        let conn = open_test_db();
        upsert_icp_patterns(&conn, &[]).unwrap();
        let result = lookup_icp_pattern(&conn, "anything").unwrap();
        assert_eq!(result, None);
    }

    // ── icp_patterns sha256 index (sanity check) ─────────────────────────────

    #[test]
    fn lookup_is_case_sensitive() {
        // Verifies that the sha256 lookup is case-sensitive (hashes are lowercase hex).
        let conn = open_test_db();
        upsert_icp_patterns(&conn, &[make_entry(1, "abcdef", "high")]).unwrap();
        // Uppercase version must not match.
        let result = lookup_icp_pattern(&conn, "ABCDEF").unwrap();
        assert_eq!(result, None);
    }
}
