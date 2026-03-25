//! Parallel chunked HTTP download engine.
//! Splits a file into N byte ranges and fetches them concurrently.
//! The caller is responsible for falling back to single-stream if the server
//! does not advertise Accept-Ranges support.

use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;
use tokio::task::JoinSet;

/// Free-tier maximum concurrent chunks.
pub const FREE_TIER_CHUNKS: usize = 4;
/// Minimum chunk size in bytes — files smaller than this use a single chunk.
pub const MIN_CHUNK_BYTES: u64 = 1_048_576; // 1 MB

/// Downloads a file using parallel HTTP range requests.
///
/// Chunk 0 is streamed directly from `probe` (the already-open GET response),
/// avoiding a wasted connection teardown and reconnect. Chunks 1..N are fetched
/// with Range GETs. Writes directly to non-overlapping regions of a pre-allocated
/// file. Emits `download://progress/{id}` events every 200 ms. If any chunk fails,
/// all remaining chunks are cancelled and an error is returned.
///
/// Args:
///   url:        The HTTP/HTTPS URL to download.
///   path:       Destination file path — will be created or truncated.
///   total_size: Exact file size in bytes (from Content-Length).
///   probe:      Already-open GET response — reused as chunk 0.
///   id:         The downloads table row id (used in event names).
///   app:        Tauri app handle for emitting progress events.
///   client:     Shared reqwest client for chunk 1..N requests.
///
/// Returns:
///   Ok(()) when all chunks have been written. Err(message) if any chunk fails.
pub async fn download_chunked(
    url: String,
    path: std::path::PathBuf,
    total_size: u64,
    probe: reqwest::Response,
    id: i64,
    app: tauri::AppHandle,
    client: reqwest::Client,
) -> Result<(), String> {
    let chunk_count =
        ((total_size / MIN_CHUNK_BYTES) as usize).clamp(1, FREE_TIER_CHUNKS);
    let chunk_size = total_size / chunk_count as u64;

    // Pre-allocate file so all chunk tasks can seek into any region
    {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        file.set_len(total_size).map_err(|e| e.to_string())?;
    }

    let total_downloaded = Arc::new(AtomicU64::new(0));

    // Background reporter — samples the counter every 200ms and emits progress events
    let reporter_dl = Arc::clone(&total_downloaded);
    let reporter_app = app.clone();
    let reporter = tokio::spawn(async move {
        let mut last_bytes: u64 = 0;
        let mut last_tick = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let now_bytes = reporter_dl.load(Ordering::Relaxed);
            let elapsed = last_tick.elapsed().as_secs_f64();
            let speed_bps = if elapsed > 0.0 {
                ((now_bytes - last_bytes) as f64 / elapsed) as u64
            } else {
                0
            };
            last_bytes = now_bytes;
            last_tick = Instant::now();
            reporter_app
                .emit_all(
                    &format!("download://progress/{id}"),
                    super::ProgressPayload {
                        downloaded: now_bytes,
                        total: Some(total_size),
                        speed_bps,
                    },
                )
                .ok();
        }
    });

    // Spawn one task per chunk.
    // Chunk 0 reuses the probe response — no extra connection needed.
    // Chunks 1..N-1 send Range GETs in parallel.
    let mut set: JoinSet<Result<(), String>> = JoinSet::new();

    set.spawn(download_chunk_from_stream(
        probe,
        path.clone(),
        0,
        chunk_size,
        Arc::clone(&total_downloaded),
    ));

    for i in 1..chunk_count {
        let start = i as u64 * chunk_size;
        let end = if i == chunk_count - 1 {
            total_size - 1
        } else {
            start + chunk_size - 1
        };
        set.spawn(download_chunk(
            url.clone(),
            path.clone(),
            start,
            end,
            client.clone(),
            Arc::clone(&total_downloaded),
        ));
    }

    // Collect results — abort all on first failure
    let mut first_error: Option<String> = None;
    while let Some(result) = set.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                first_error = Some(e);
                set.abort_all();
                break;
            }
            Err(join_err) => {
                first_error = Some(join_err.to_string());
                set.abort_all();
                break;
            }
        }
    }

    reporter.abort();

    if let Some(e) = first_error {
        return Err(e);
    }

    // Emit a final 100% progress event
    app.emit_all(
        &format!("download://progress/{id}"),
        super::ProgressPayload {
            downloaded: total_size,
            total: Some(total_size),
            speed_bps: 0,
        },
    )
    .ok();

    Ok(())
}

/// Reads exactly `bytes_to_read` bytes from an already-open response stream and
/// writes them at `start` in the pre-allocated file. Used to pipe the probe GET
/// response as chunk 0, avoiding a redundant TCP/TLS connection setup.
///
/// Args:
///   response:      In-flight GET response (no Range header — full file body).
///   path:          Pre-allocated destination file path.
///   start:         File offset to begin writing (0 for chunk 0).
///   bytes_to_read: Exact byte count to consume (= chunk_size for chunk 0).
///   total_dl:      Shared atomic byte counter — incremented on each write.
///
/// Returns:
///   Ok(()) on success. Err(message) on any I/O or network error.
async fn download_chunk_from_stream(
    mut response: reqwest::Response,
    path: std::path::PathBuf,
    start: u64,
    bytes_to_read: u64,
    total_dl: Arc<AtomicU64>,
) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, file);
    tokio::task::block_in_place(|| {
        writer.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())
    })?;

    let mut remaining = bytes_to_read;
    while remaining > 0 {
        let chunk = match response.chunk().await.map_err(|e| e.to_string())? {
            Some(c) => c,
            None => break,
        };
        let to_write = (chunk.len() as u64).min(remaining) as usize;
        tokio::task::block_in_place(|| {
            writer.write_all(&chunk[..to_write]).map_err(|e| e.to_string())
        })?;
        total_dl.fetch_add(to_write as u64, Ordering::Relaxed);
        remaining -= to_write as u64;
    }

    tokio::task::block_in_place(|| writer.flush().map_err(|e| e.to_string()))?;
    Ok(())
}

/// Fetches a single byte range and writes it to the correct offset in the file.
///
/// Args:
///   url:       The HTTP/HTTPS URL.
///   path:      Pre-allocated destination file (must already exist).
///   start:     First byte of the range (inclusive).
///   end:       Last byte of the range (inclusive).
///   client:    Shared reqwest client.
///   total_dl:  Shared atomic byte counter — incremented on each write.
///
/// Returns:
///   Ok(()) on success. Err(message) on any HTTP or I/O error.
async fn download_chunk(
    url: String,
    path: std::path::PathBuf,
    start: u64,
    end: u64,
    client: reqwest::Client,
    total_dl: Arc<AtomicU64>,
) -> Result<(), String> {
    let response = client
        .get(&url)
        .header("Range", format!("bytes={start}-{end}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status();
    if !status.is_success() && status.as_u16() != 206 {
        return Err(format!("HTTP {status} for chunk {start}-{end}"));
    }

    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, file);
    tokio::task::block_in_place(|| {
        writer.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())
    })?;

    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        let len = chunk.len() as u64;
        tokio::task::block_in_place(|| {
            writer.write_all(&chunk).map_err(|e| e.to_string())
        })?;
        total_dl.fetch_add(len, Ordering::Relaxed);
    }

    tokio::task::block_in_place(|| writer.flush().map_err(|e| e.to_string()))?;
    Ok(())
}
