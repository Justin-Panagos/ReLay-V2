use sha2::{Digest, Sha256};
use std::io::Read;

use super::{LayerResult, LayerVerdict, ScanContext};
use tauri::Manager;

use crate::db::{self, DbState};

/// Layer 1: SHA-256 hash check.
/// Computes the file's SHA-256 digest and stores it in `ctx.sha256`.
/// Checks the local ICP pattern cache first, then the VirusTotal API (if configured).
/// Returns Threat immediately if a match is found in either source; Clean otherwise.
///
/// Args:
///   ctx:    Mutable scan context — `sha256` field is populated by this layer.
///   vt_key: Optional VirusTotal API key from settings.
///   app:    Tauri app handle used to access the shared DbState for ICP cache lookup.
///
/// Returns:
///   LayerResult with the hash verdict.
pub async fn scan(
    ctx: &mut ScanContext,
    vt_key: Option<&str>,
    app: &tauri::AppHandle,
) -> LayerResult {
    // ── Step 1: compute SHA-256 ───────────────────────────────────────────────
    let sha256 = match compute_sha256(&ctx.path) {
        Ok(h) => h,
        Err(e) => {
            return LayerResult {
                layer: 1,
                name: "Hash",
                verdict: LayerVerdict::Suspicious {
                    reason: format!("could not hash file: {e}"),
                },
            };
        }
    };
    ctx.sha256 = sha256.clone();

    // ── Step 2: check local ICP pattern cache ────────────────────────────────
    if let Some(db_state) = app.try_state::<DbState>() {
        if let Ok(conn) = db_state.0.lock() {
            if let Ok(Some(threat_level)) = db::lookup_icp_pattern(&conn, &sha256) {
                return LayerResult {
                    layer: 1,
                    name: "Hash",
                    verdict: LayerVerdict::Threat {
                        reason: format!(
                            "ICP Shield Network: known threat ({threat_level})"
                        ),
                    },
                };
            }
        }
    }

    // ── Step 3: optional VirusTotal lookup ────────────────────────────────────
    if let Some(key) = vt_key {
        let client = app
            .try_state::<reqwest::Client>()
            .map(|s| s.inner().clone())
            .unwrap_or_else(reqwest::Client::new);
        match check_virustotal(&sha256, key, &client).await {
            Ok(Some(reason)) => {
                return LayerResult {
                    layer: 1,
                    name: "Hash",
                    verdict: LayerVerdict::Threat { reason },
                };
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("[shield] VirusTotal lookup failed: {e}");
                // VT error (rate-limit, bad key, network) — do not treat as Clean silently.
                // Return Suspicious so the scan log shows the failure.
                return LayerResult {
                    layer: 1,
                    name: "Hash",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("VirusTotal check failed: {e}"),
                    },
                };
            }
        }
    }

    LayerResult {
        layer: 1,
        name: "Hash",
        verdict: LayerVerdict::Clean,
    }
}

/// Computes the SHA-256 digest of a file and returns it as a lowercase hex string.
///
/// Args:
///   path: Path to the file to hash.
///
/// Returns:
///   Hex-encoded SHA-256 string, or an error message.
fn compute_sha256(path: &std::path::Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Queries the VirusTotal v3 API for a file hash report using the provided shared client.
/// Returns `Some(reason)` if the file is flagged as malicious, `None` if clean or not found,
/// or an error if the request fails or VT returns a non-success/non-404 status.
///
/// Args:
///   sha256:  Hex-encoded SHA-256 of the file.
///   api_key: VirusTotal API key.
///   client:  Shared reqwest client (avoids a new TLS handshake per call).
///
/// Returns:
///   Ok(Some(reason)) if malicious detections > 0.
///   Ok(None) if the hash is not found (404) or clean.
///   Err on network failure or non-success/non-404 HTTP status (rate-limit, bad key, etc.).
async fn check_virustotal(
    sha256: &str,
    api_key: &str,
    client: &reqwest::Client,
) -> Result<Option<String>, String> {
    let url = format!("https://www.virustotal.com/api/v3/files/{sha256}");
    let resp = client
        .get(&url)
        .header("x-apikey", api_key)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if resp.status().as_u16() == 404 {
        return Ok(None);
    }

    if !resp.status().is_success() {
        return Err(format!("VirusTotal API error: HTTP {}", resp.status()));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let malicious = json
        .pointer("/data/attributes/last_analysis_stats/malicious")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if malicious > 0 {
        Ok(Some(format!(
            "VirusTotal: {malicious} engine(s) flagged this file"
        )))
    } else {
        Ok(None)
    }
}
