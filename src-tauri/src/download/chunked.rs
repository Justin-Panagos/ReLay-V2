//! Parallel chunked HTTP download engine.
//! Splits a file into N byte ranges and fetches them concurrently.
//! Supports pause via CancellationToken: each chunk yields cleanly when cancelled,
//! allowing byte-offset snapshots to be saved for resume.

use crate::db::ChunkSnapshot;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Free-tier maximum concurrent chunks.
pub const FREE_TIER_CHUNKS: usize = 16;
/// Pro-tier maximum concurrent chunks.
pub const PRO_TIER_CHUNKS: usize = 32;
/// Minimum chunk size in bytes — files smaller than this use a single chunk.
pub const MIN_CHUNK_BYTES: u64 = 1_048_576; // 1 MB

/// Outcome of a single chunk task.
enum ChunkStatus {
    Complete,
    Paused,
}

/// Outcome returned by `download_chunked` to `run_download`.
pub enum LifecycleOutcome {
    /// All chunks completed successfully.
    Complete,
    /// Download was interrupted (pause or cancel) — snapshots contain per-chunk byte counts.
    Paused(Vec<ChunkSnapshot>),
}

/// Downloads a file using parallel HTTP range requests, with cancellation support.
///
/// Chunk 0 is streamed directly from `probe` (the already-open GET response),
/// avoiding a wasted connection teardown and reconnect. Chunks 1..N are fetched
/// with Range GETs. Writes directly to non-overlapping regions of a pre-allocated
/// file. Emits `download://progress/{id}` events every 200 ms.
///
/// If `token` is cancelled, all chunk tasks yield cleanly and the function returns
/// `LifecycleOutcome::Paused` with per-chunk byte-offset snapshots.
///
/// If `resume_offsets` is `Some`, each chunk resumes from `start_byte + written_bytes`
/// instead of `start_byte`, skipping already-written data.
///
/// Args:
///   url:            The HTTP/HTTPS URL to download.
///   path:           Destination file path — must be pre-allocated or will be truncated.
///   total_size:     Exact file size in bytes (from Content-Length).
///   probe:          Already-open GET response — reused as chunk 0.
///   id:             The downloads table row id (used in event names).
///   app:            Tauri app handle for emitting progress events.
///   client:         Shared reqwest client for chunk 1..N requests.
///   token:          CancellationToken — fire to pause or cancel this download.
///   resume_offsets: Optional per-chunk snapshots from a previous paused session.
///   max_chunks:     Upper bound on chunk count — pass FREE_TIER_CHUNKS or PRO_TIER_CHUNKS.
///   bandwidth:      Bandwidth limit in kbps shared across all chunks (0 = unlimited).
///
/// Returns:
///   Ok(LifecycleOutcome) on clean finish or pause. Err(message) if any chunk errors.
#[allow(clippy::too_many_arguments)]
pub async fn download_chunked(
    url: String,
    path: std::path::PathBuf,
    total_size: u64,
    probe: reqwest::Response,
    id: i64,
    app: tauri::AppHandle,
    client: reqwest::Client,
    token: CancellationToken,
    resume_offsets: Option<Vec<ChunkSnapshot>>,
    max_chunks: usize,
    bandwidth: Arc<AtomicU32>,
) -> Result<LifecycleOutcome, String> {
    let chunk_count =
        ((total_size / MIN_CHUNK_BYTES) as usize).clamp(1, max_chunks);
    let chunk_size = total_size / chunk_count as u64;

    // Build the (start, end) plan for each chunk.
    let chunk_plan: Vec<(u64, u64)> = (0..chunk_count)
        .map(|i| {
            let start = i as u64 * chunk_size;
            let end = if i == chunk_count - 1 {
                total_size - 1
            } else {
                start + chunk_size - 1
            };
            (start, end)
        })
        .collect();

    // Pre-allocate file on a fresh download; on resume the file already exists with data.
    if resume_offsets.is_none() {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        file.set_len(total_size).map_err(|e| e.to_string())?;
    }

    // Per-chunk byte counters — read after JoinSet drains to build snapshots.
    let per_chunk_written: Vec<Arc<AtomicU64>> = (0..chunk_count)
        .map(|i| {
            // Pre-seed resume amounts so the global counter starts from the right baseline.
            let initial = resume_offsets
                .as_ref()
                .and_then(|offsets| offsets.get(i))
                .map(|s| s.written_bytes)
                .unwrap_or(0);
            Arc::new(AtomicU64::new(initial))
        })
        .collect();

    // Global total_downloaded — sum of per-chunk counters, for progress events.
    let total_downloaded = Arc::new(AtomicU64::new(
        resume_offsets
            .as_ref()
            .map(|offsets| offsets.iter().map(|s| s.written_bytes).sum())
            .unwrap_or(0),
    ));

    // Background reporter — samples the global counter every 200ms and emits progress events.
    let reporter_dl = Arc::clone(&total_downloaded);
    let reporter_app = app.clone();
    let reporter_token = token.clone();
    let reporter = tokio::spawn(async move {
        let mut last_bytes: u64 = 0;
        let mut last_tick = Instant::now();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = reporter_token.cancelled() => break,
            }
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
    let mut set: JoinSet<Result<ChunkStatus, String>> = JoinSet::new();

    // Chunk 0: resume from probe response (or adjust start if resuming).
    let chunk0_written = resume_offsets
        .as_ref()
        .and_then(|o| o.first())
        .map(|s| s.written_bytes)
        .unwrap_or(0);
    let (chunk0_start, chunk0_end) = chunk_plan[0];
    let chunk0_size = chunk0_end - chunk0_start + 1;
    if chunk0_written > chunk0_size {
        return Err(format!(
            "corrupted resume snapshot: chunk 0 written_bytes ({chunk0_written}) \
             exceeds chunk size ({chunk0_size}) — restarting download from scratch"
        ));
    }
    let chunk0_bytes_to_read = chunk0_size - chunk0_written;

    if chunk0_bytes_to_read > 0 {
        set.spawn(download_chunk_from_stream(
            probe,
            path.clone(),
            chunk0_start + chunk0_written,
            chunk0_bytes_to_read,
            Arc::clone(&total_downloaded),
            Arc::clone(&per_chunk_written[0]),
            token.clone(),
            Arc::clone(&bandwidth),
            chunk_count,
        ));
    } else {
        // Chunk 0 already fully written — drop the probe response.
        drop(probe);
        set.spawn(async { Ok(ChunkStatus::Complete) });
    }

    // Chunks 1..N: Range GETs, adjusted for any previously written bytes.
    for i in 1..chunk_count {
        let (start, end) = chunk_plan[i];
        let already_written = resume_offsets
            .as_ref()
            .and_then(|o| o.get(i))
            .map(|s| s.written_bytes)
            .unwrap_or(0);
        let adjusted_start = start + already_written;

        if adjusted_start > end {
            // Already complete — skip.
            set.spawn(async { Ok(ChunkStatus::Complete) });
        } else {
            set.spawn(download_chunk(
                url.clone(),
                path.clone(),
                adjusted_start,
                end,
                client.clone(),
                Arc::clone(&total_downloaded),
                Arc::clone(&per_chunk_written[i]),
                token.clone(),
                Arc::clone(&bandwidth),
                chunk_count,
            ));
        }
    }

    // Collect results.
    let mut any_paused = false;
    let mut first_error: Option<String> = None;

    while let Some(result) = set.join_next().await {
        match result {
            Ok(Ok(ChunkStatus::Complete)) => {}
            Ok(Ok(ChunkStatus::Paused)) => {
                any_paused = true;
            }
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

    if any_paused {
        // Build snapshots from chunk plan + per-chunk byte counters.
        let snapshots: Vec<ChunkSnapshot> = chunk_plan
            .iter()
            .zip(per_chunk_written.iter())
            .enumerate()
            .map(|(i, ((start, end), written))| ChunkSnapshot {
                chunk_idx: i,
                start_byte: *start,
                end_byte: *end,
                written_bytes: written.load(Ordering::Relaxed),
            })
            .collect();
        return Ok(LifecycleOutcome::Paused(snapshots));
    }

    // Emit a final 100% progress event.
    app.emit_all(
        &format!("download://progress/{id}"),
        super::ProgressPayload {
            downloaded: total_size,
            total: Some(total_size),
            speed_bps: 0,
        },
    )
    .ok();

    Ok(LifecycleOutcome::Complete)
}

/// Reads up to `bytes_to_read` bytes from an already-open response stream and
/// writes them at `start` in the pre-allocated file.
///
/// Yields cleanly when `token` is cancelled, returning `ChunkStatus::Paused`.
///
/// Args:
///   response:      In-flight GET response (no Range header — full file body).
///   path:          Pre-allocated destination file path.
///   start:         File offset to begin writing.
///   bytes_to_read: Exact byte count to consume.
///   total_dl:      Shared global byte counter — incremented on each write.
///   per_chunk_dl:  Per-chunk byte counter — incremented on each write.
///   token:         CancellationToken — pause/cancel signal.
///   bandwidth:     Shared bandwidth limit in kbps (0 = unlimited).
///   chunk_count:   Total number of active chunks — used to compute per-chunk budget.
///
/// Returns:
///   Ok(ChunkStatus) on success or pause. Err(message) on any I/O or network error.
#[allow(clippy::too_many_arguments)]
async fn download_chunk_from_stream(
    mut response: reqwest::Response,
    path: std::path::PathBuf,
    start: u64,
    bytes_to_read: u64,
    total_dl: Arc<AtomicU64>,
    per_chunk_dl: Arc<AtomicU64>,
    token: CancellationToken,
    bandwidth: Arc<AtomicU32>,
    chunk_count: usize,
) -> Result<ChunkStatus, String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, file);
    tokio::task::block_in_place(|| {
        writer.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())
    })?;

    let mut remaining = bytes_to_read;
    let mut chunk_bytes_this_window: u64 = 0;
    let mut window_start = Instant::now();

    while remaining > 0 {
        let maybe_chunk = tokio::select! {
            result = response.chunk() => result.map_err(|e| e.to_string())?,
            _ = token.cancelled() => {
                tokio::task::block_in_place(|| writer.flush().ok());
                return Ok(ChunkStatus::Paused);
            }
        };

        let chunk = match maybe_chunk {
            Some(c) => c,
            None => break,
        };
        let to_write = (chunk.len() as u64).min(remaining) as usize;
        tokio::task::block_in_place(|| {
            writer.write_all(&chunk[..to_write]).map_err(|e| e.to_string())
        })?;
        let written = to_write as u64;
        total_dl.fetch_add(written, Ordering::Relaxed);
        per_chunk_dl.fetch_add(written, Ordering::Relaxed);
        remaining -= written;

        // Throttle: compute per-chunk budget and sleep if ahead.
        let kbps = bandwidth.load(Ordering::Relaxed);
        if kbps > 0 {
            chunk_bytes_this_window += written;
            let budget_bps = (kbps as u64 * 1024) / chunk_count.max(1) as u64;
            let elapsed = window_start.elapsed().as_secs_f64();
            let expected_secs = chunk_bytes_this_window as f64 / budget_bps as f64;
            if expected_secs > elapsed {
                let sleep_ms = ((expected_secs - elapsed) * 1000.0) as u64;
                if sleep_ms > 0 {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {}
                        _ = token.cancelled() => {
                            tokio::task::block_in_place(|| writer.flush().ok());
                            return Ok(ChunkStatus::Paused);
                        }
                    }
                }
            }
            // Reset window every second to keep floating-point error bounded.
            if window_start.elapsed().as_secs_f64() >= 1.0 {
                chunk_bytes_this_window = 0;
                window_start = Instant::now();
            }
        }
    }

    tokio::task::block_in_place(|| writer.flush().map_err(|e| e.to_string()))?;
    Ok(ChunkStatus::Complete)
}

/// Fetches a single byte range and writes it to the correct offset in the file.
///
/// Yields cleanly when `token` is cancelled, returning `ChunkStatus::Paused`.
///
/// Args:
///   url:          The HTTP/HTTPS URL.
///   path:         Pre-allocated destination file (must already exist).
///   start:        First byte of the range (inclusive, adjusted for resume).
///   end:          Last byte of the range (inclusive).
///   client:       Shared reqwest client.
///   total_dl:     Shared global byte counter — incremented on each write.
///   per_chunk_dl: Per-chunk byte counter — incremented on each write.
///   token:        CancellationToken — pause/cancel signal.
///   bandwidth:    Shared bandwidth limit in kbps (0 = unlimited).
///   chunk_count:  Total number of active chunks — used to compute per-chunk budget.
///
/// Returns:
///   Ok(ChunkStatus) on success or pause. Err(message) on any HTTP or I/O error.
#[allow(clippy::too_many_arguments)]
async fn download_chunk(
    url: String,
    path: std::path::PathBuf,
    start: u64,
    end: u64,
    client: reqwest::Client,
    total_dl: Arc<AtomicU64>,
    per_chunk_dl: Arc<AtomicU64>,
    token: CancellationToken,
    bandwidth: Arc<AtomicU32>,
    chunk_count: usize,
) -> Result<ChunkStatus, String> {
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
    let mut chunk_bytes_this_window: u64 = 0;
    let mut window_start = Instant::now();

    loop {
        let maybe_chunk = tokio::select! {
            result = response.chunk() => result.map_err(|e| e.to_string())?,
            _ = token.cancelled() => {
                tokio::task::block_in_place(|| writer.flush().ok());
                return Ok(ChunkStatus::Paused);
            }
        };

        let chunk = match maybe_chunk {
            Some(c) => c,
            None => break,
        };
        let len = chunk.len() as u64;
        tokio::task::block_in_place(|| {
            writer.write_all(&chunk).map_err(|e| e.to_string())
        })?;
        total_dl.fetch_add(len, Ordering::Relaxed);
        per_chunk_dl.fetch_add(len, Ordering::Relaxed);

        // Throttle: compute per-chunk budget and sleep if ahead.
        let kbps = bandwidth.load(Ordering::Relaxed);
        if kbps > 0 {
            chunk_bytes_this_window += len;
            let budget_bps = (kbps as u64 * 1024) / chunk_count.max(1) as u64;
            let elapsed = window_start.elapsed().as_secs_f64();
            let expected_secs = chunk_bytes_this_window as f64 / budget_bps as f64;
            if expected_secs > elapsed {
                let sleep_ms = ((expected_secs - elapsed) * 1000.0) as u64;
                if sleep_ms > 0 {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {}
                        _ = token.cancelled() => {
                            tokio::task::block_in_place(|| writer.flush().ok());
                            return Ok(ChunkStatus::Paused);
                        }
                    }
                }
            }
            if window_start.elapsed().as_secs_f64() >= 1.0 {
                chunk_bytes_this_window = 0;
                window_start = Instant::now();
            }
        }
    }

    tokio::task::block_in_place(|| writer.flush().map_err(|e| e.to_string()))?;
    Ok(ChunkStatus::Complete)
}

#[cfg(test)]
mod tests {
    use crate::db::ChunkSnapshot;

    fn make_snapshot(chunk_idx: usize, start_byte: u64, end_byte: u64, written_bytes: u64) -> ChunkSnapshot {
        ChunkSnapshot { chunk_idx, start_byte, end_byte, written_bytes }
    }

    #[test]
    fn resume_adjusted_start_adds_written_bytes() {
        let start: u64 = 1_000_000;
        let end:   u64 = 2_000_000;
        let snap = make_snapshot(1, start, end, 250_000);
        // Mirrors download_chunked: adjusted_start = start + already_written
        let adjusted_start = snap.start_byte + snap.written_bytes;
        assert_eq!(adjusted_start, 1_250_000);
        assert!(adjusted_start <= end, "partial chunk should not be skipped");
    }

    #[test]
    fn resume_chunk_skipped_when_fully_written() {
        let start: u64 = 1_000_000;
        let end:   u64 = 2_000_000;
        // HTTP Range header is inclusive (bytes=start-end), so a complete chunk
        // has written (end - start + 1) bytes, giving adjusted_start = end + 1.
        let fully_written = end - start + 1;
        let snap = make_snapshot(1, start, end, fully_written);
        let adjusted_start = snap.start_byte + snap.written_bytes;
        // The loop condition is `adjusted_start > end` → spawns a no-op task.
        assert!(adjusted_start > end, "fully-written chunk should be skipped");
    }
}
