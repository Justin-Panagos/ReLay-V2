//! Native Messaging host for the ReLay Chrome extension.
//!
//! Runs as a standalone stdio loop (no Tauri stack) when the binary is
//! launched by Chrome with `--native-messaging`. All I/O uses the Chrome
//! Native Messaging wire format: a 4-byte little-endian `u32` length
//! prefix followed by a UTF-8 JSON payload.
//!
//! The module also exposes `install_host_manifest()`, called from
//! `main.rs` setup, which writes the platform-specific JSON manifest (and
//! on Windows the registry key) that tells Chrome where to find the binary.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

// ── Wire format ───────────────────────────────────────────────────────────────

/// A message received from the Chrome extension over stdin.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InboundMessage {
    /// Enqueue a new download; Chrome has already cancelled its own download.
    StartDownload { url: String, filename: String },
    /// Poll current download state for the popup.
    GetStatus,
}

/// A message sent back to the extension over stdout.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutboundMessage {
    /// Acknowledgement of a StartDownload; carries the new row ID.
    Ack { id: i64 },
    /// Current list of active downloads for the popup.
    Status { downloads: Vec<DownloadStatus> },
    /// Unrecoverable error description.
    Error { message: String },
}

/// A single download row returned by GetStatus.
#[derive(Serialize)]
struct DownloadStatus {
    id: i64,
    filename: String,
    status: String,
    /// Bytes transferred so far.
    downloaded: i64,
    /// Total file size, None when unknown.
    total: Option<i64>,
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Main loop for `--native-messaging` mode.
///
/// Reads length-prefixed JSON messages from stdin, dispatches each to the
/// appropriate handler, and writes the response back to stdout. Returns when
/// Chrome closes the pipe (i.e. `read_exact` returns an error).
pub fn run() {
    let stdin  = io::stdin();
    let stdout = io::stdout();

    let conn = match open_db() {
        Ok(c)  => c,
        Err(e) => {
            let _ = write_message(&stdout, &OutboundMessage::Error { message: e });
            return;
        }
    };

    let mut handle = stdin.lock();

    loop {
        // Read the 4-byte LE length prefix.
        let mut len_buf = [0u8; 4];
        if handle.read_exact(&mut len_buf).is_err() {
            // Pipe closed — Chrome shut down or the extension was unloaded.
            break;
        }
        let len = u32::from_le_bytes(len_buf) as usize;

        // Read exactly `len` bytes of JSON.
        let mut payload = vec![0u8; len];
        if handle.read_exact(&mut payload).is_err() {
            break;
        }

        let response = match serde_json::from_slice::<InboundMessage>(&payload) {
            Ok(InboundMessage::StartDownload { url, filename }) => {
                handle_start_download(&conn, url, filename)
            }
            Ok(InboundMessage::GetStatus) => handle_get_status(&conn),
            Err(e) => OutboundMessage::Error {
                message: format!("parse error: {e}"),
            },
        };

        let _ = write_message(&stdout, &response);
    }
}

/// Writes the platform-specific native host manifest (and on Windows the
/// registry key) so Chrome knows where to find this binary.
///
/// Called once during normal Tauri startup; it is idempotent so repeated
/// launches are safe. Errors are non-fatal — they are logged to stderr and
/// the GUI continues.
///
/// Returns:
///   Ok(()) on success, Err(String) with a human-readable description on
///   failure.
pub fn install_host_manifest() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not resolve exe path: {e}"))?;

    let manifest = build_manifest_json(&exe);

    write_manifest(&manifest)
}

// ── Message handlers ──────────────────────────────────────────────────────────

/// Inserts a new queued download row for `url`/`filename` and brings the
/// ReLay GUI to the front.
///
/// Args:
///   conn:     Open database connection.
///   url:      The URL to download.
///   filename: The suggested filename.
///
/// Returns:
///   `Ack { id }` on success, `Error { message }` on failure.
fn handle_start_download(conn: &Connection, url: String, filename: String) -> OutboundMessage {
    let destination = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'default_folder'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| {
            dirs::download_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .to_string_lossy()
                .to_string()
        });

    let now = chrono_now();

    let result = conn.execute(
        "INSERT INTO downloads
             (url, filename, destination, status, downloaded_bytes, created_at, type)
         VALUES (?1, ?2, ?3, 'queued', 0, ?4, 'http')",
        params![url, filename, destination, now],
    );

    match result {
        Ok(_) => {
            let id = conn.last_insert_rowid();
            bring_gui_to_front();
            OutboundMessage::Ack { id }
        }
        Err(e) => OutboundMessage::Error {
            message: format!("db insert failed: {e}"),
        },
    }
}

/// Queries the database for all active (downloading / queued / paused)
/// downloads and returns them as a Status message.
///
/// Args:
///   conn: Open database connection.
///
/// Returns:
///   `Status { downloads }` with up to 20 entries, newest first.
fn handle_get_status(conn: &Connection) -> OutboundMessage {
    let mut stmt = match conn.prepare(
        "SELECT id, filename, status, downloaded_bytes, size_bytes
         FROM downloads
         WHERE status IN ('downloading', 'queued', 'paused')
         ORDER BY created_at DESC
         LIMIT 20",
    ) {
        Ok(s)  => s,
        Err(e) => {
            return OutboundMessage::Error {
                message: format!("db prepare failed: {e}"),
            }
        }
    };

    let rows = stmt.query_map([], |row| {
        Ok(DownloadStatus {
            id:         row.get(0)?,
            filename:   row.get(1)?,
            status:     row.get(2)?,
            downloaded: row.get(3)?,
            total:      row.get(4)?,
        })
    });

    let downloads: Vec<DownloadStatus> = match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(e)   => {
            return OutboundMessage::Error {
                message: format!("db query failed: {e}"),
            }
        }
    };

    OutboundMessage::Status { downloads }
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Serialises `msg` to JSON and writes it to `out` with a 4-byte LE length
/// prefix, flushing immediately.
///
/// Args:
///   out: Locked stdout handle.
///   msg: Message to serialise and send.
///
/// Returns:
///   `io::Result<()>` — errors are typically swallowed by the caller since a
///   broken pipe means Chrome has already gone away.
fn write_message<W: Write>(mut out: W, msg: &OutboundMessage) -> io::Result<()> {
    let json  = serde_json::to_vec(msg).unwrap_or_else(|_| b"{}".to_vec());
    let len   = (json.len() as u32).to_le_bytes();
    out.write_all(&len)?;
    out.write_all(&json)?;
    out.flush()
}

// ── Database helpers ──────────────────────────────────────────────────────────

/// Opens the ReLay SQLite database in WAL mode for safe concurrent access
/// alongside the GUI process.
///
/// Returns:
///   Open Connection, or Err(String) if the path cannot be resolved or the
///   file cannot be opened.
fn open_db() -> Result<Connection, String> {
    let db_path = data_dir()?.join("relay.db");

    let conn = Connection::open(&db_path)
        .map_err(|e| format!("cannot open db at {}: {e}", db_path.display()))?;

    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .map_err(|e| format!("WAL pragma failed: {e}"))?;

    Ok(conn)
}

/// Returns the platform-specific app data directory used by the Tauri process
/// (`{data_dir}/com.relay.app`).
///
/// Returns:
///   The resolved PathBuf, or Err(String) if the OS data dir is unavailable.
fn data_dir() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|d| d.join("com.relay.app"))
        .ok_or_else(|| "cannot resolve data dir".to_string())
}

// ── GUI activation ────────────────────────────────────────────────────────────

/// Brings the ReLay GUI window to the front.
///
/// On macOS uses `open -a ReLay`; on Windows re-spawns the current
/// executable (Tauri handles single-instance focus); on Linux uses
/// `xdg-open` with a no-op URL to hint the window manager.
/// Errors are silently ignored — the download is already queued.
fn bring_gui_to_front() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-a", "ReLay"])
            .spawn();
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }
}

// ── Manifest installation ─────────────────────────────────────────────────────

/// Builds the Chrome Native Messaging host manifest JSON string.
///
/// Args:
///   exe: Absolute path to the ReLay binary.
///
/// Returns:
///   A JSON string suitable for writing to the manifest file.
fn build_manifest_json(exe: &std::path::Path) -> String {
    let exe_str = exe.to_string_lossy().replace('\\', "\\\\");
    format!(
        r#"{{
  "name": "com.relay.app",
  "description": "ReLay download manager native host",
  "path": "{exe_str}",
  "type": "stdio",
  "allowed_origins": [
    "chrome-extension://clicbaegmppeombaglajpeikmbfojobo/"
  ]
}}"#
    )
}

/// Writes the native host manifest to the correct OS-specific location.
///
/// On Windows also creates the registry key that Chrome requires.
///
/// Args:
///   manifest: JSON string to write.
///
/// Returns:
///   Ok(()) on success, Err(String) on failure.
fn write_manifest(manifest: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        write_manifest_macos(manifest)
    }

    #[cfg(target_os = "windows")]
    {
        write_manifest_windows(manifest)
    }

    #[cfg(target_os = "linux")]
    {
        write_manifest_linux(manifest)
    }
}

#[cfg(target_os = "macos")]
/// Writes the native host manifest for Chrome on macOS.
///
/// Args:
///   manifest: JSON string to write.
///
/// Returns:
///   Ok(()) on success, Err(String) on failure.
fn write_manifest_macos(manifest: &str) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("cannot resolve home dir")?;
    let hosts_dir = home
        .join("Library/Application Support/Google/Chrome/NativeMessagingHosts");

    std::fs::create_dir_all(&hosts_dir)
        .map_err(|e| format!("create hosts dir: {e}"))?;

    let path = hosts_dir.join("com.relay.app.json");
    std::fs::write(&path, manifest)
        .map_err(|e| format!("write manifest: {e}"))?;

    Ok(())
}

#[cfg(target_os = "linux")]
/// Writes the native host manifest for Chrome on Linux.
///
/// Args:
///   manifest: JSON string to write.
///
/// Returns:
///   Ok(()) on success, Err(String) on failure.
fn write_manifest_linux(manifest: &str) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("cannot resolve home dir")?;
    let hosts_dir = home.join(".config/google-chrome/NativeMessagingHosts");

    std::fs::create_dir_all(&hosts_dir)
        .map_err(|e| format!("create hosts dir: {e}"))?;

    let path = hosts_dir.join("com.relay.app.json");
    std::fs::write(&path, manifest)
        .map_err(|e| format!("write manifest: {e}"))?;

    Ok(())
}

#[cfg(target_os = "windows")]
/// Writes the native host manifest and registry key for Chrome on Windows.
///
/// The JSON file is written to `%APPDATA%\ReLay\NativeMessaging\` and the
/// registry key `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.relay.app`
/// is set to that path so Chrome can locate the manifest.
///
/// Args:
///   manifest: JSON string to write.
///
/// Returns:
///   Ok(()) on success, Err(String) on failure.
fn write_manifest_windows(manifest: &str) -> Result<(), String> {
    let appdata = dirs::data_dir().ok_or("cannot resolve APPDATA")?;
    let manifest_dir = appdata.join("ReLay").join("NativeMessaging");

    std::fs::create_dir_all(&manifest_dir)
        .map_err(|e| format!("create manifest dir: {e}"))?;

    let manifest_path = manifest_dir.join("com.relay.app.json");
    std::fs::write(&manifest_path, manifest)
        .map_err(|e| format!("write manifest: {e}"))?;

    // Write the registry key that Chrome checks on Windows.
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(r"Software\Google\Chrome\NativeMessagingHosts\com.relay.app")
        .map_err(|e| format!("create registry key: {e}"))?;

    key.set_value("", &manifest_path.to_string_lossy().as_ref())
        .map_err(|e| format!("set registry value: {e}"))?;

    Ok(())
}

// ── Misc utilities ────────────────────────────────────────────────────────────

/// Returns the current UTC datetime as an ISO-8601 string.
///
/// Returns:
///   String in the format `YYYY-MM-DDTHH:MM:SS`.
fn chrono_now() -> String {
    // std only — avoids pulling in chrono just for a timestamp.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Rudimentary epoch → calendar conversion (good until 2100).
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86_400;

    // Days since 1970-01-01 → Gregorian (Meeus/Jones/Butcher algorithm).
    let z = days + 2_440_588; // Julian Day Number
    let a = (z as f64 - 1_867_216.25) / 36_524.25;
    let a = z + 1 + a as u64 - (a / 4.0) as u64;
    let b = a + 1524;
    let c = ((b as f64 - 122.1) / 365.25) as u64;
    let d = (365.25 * c as f64) as u64;
    let e = ((b - d) as f64 / 30.6001) as u64;

    let day   = b - d - (30.6001 * e as f64) as u64;
    let month = if e < 14 { e - 1 } else { e - 13 };
    let year  = if month > 2 { c - 4716 } else { c - 4715 };

    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}")
}
