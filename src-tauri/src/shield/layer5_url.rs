use super::{LayerResult, LayerVerdict, ScanContext};

/// Top-level domains commonly associated with malware distribution.
const SUSPICIOUS_TLDS: &[&str] = &[
    ".tk", ".ml", ".ga", ".cf", ".gq",  // Freenom TLDs heavily abused for malware
    ".pw", ".top", ".xyz",              // Common malware TLDs
    ".ru", ".su",                       // Flagged for elevated abuse rates (heuristic only)
];

/// URL path fragments associated with malware or exploit kits.
const SUSPICIOUS_PATHS: &[&str] = &[
    "/exploit/", "/payload/", "/dropper/", "/crypter/",
    "/loader/", "/.well-known/evil", "/shellcode",
];

/// Layer 5: URL reputation check.
/// Checks the download URL host against a hardcoded blocklist of suspicious TLDs/patterns.
/// If a VirusTotal API key is set, also queries the VT URL report.
/// Returns Suspicious (never Threat) — URL reputation alone is not definitive.
///
/// Args:
///   ctx:     Scan context (url is used).
///   vt_key:  Optional VirusTotal API key.
///
/// Returns:
///   LayerResult with Suspicious or Clean.
pub async fn scan(ctx: &ScanContext, vt_key: Option<&str>) -> LayerResult {
    let url = ctx.url.clone();
    let vt_key = vt_key.map(|s| s.to_string());

    // Blocklist check is synchronous and fast.
    if let Some(reason) = check_blocklist(&url) {
        return LayerResult {
            layer: 5,
            name: "URL Reputation",
            verdict: LayerVerdict::Suspicious { reason },
        };
    }

    // Optional VT URL lookup.
    if let Some(key) = vt_key {
        match check_vt_url(&url, &key).await {
            Ok(Some(reason)) => {
                return LayerResult {
                    layer: 5,
                    name: "URL Reputation",
                    verdict: LayerVerdict::Suspicious { reason },
                };
            }
            // VT unavailable or rate-limited — treat as Suspicious so the user
            // is informed rather than silently passing a file VT could not evaluate.
            Err(reason) => {
                return LayerResult {
                    layer: 5,
                    name: "URL Reputation",
                    verdict: LayerVerdict::Suspicious { reason },
                };
            }
            Ok(None) => {}
        }
    }

    LayerResult {
        layer: 5,
        name: "URL Reputation",
        verdict: LayerVerdict::Clean,
    }
}

/// Checks the URL against static blocklists of suspicious TLDs and path fragments.
///
/// Args:
///   url: The download URL string.
///
/// Returns:
///   Some(reason) if suspicious, None if clean.
fn check_blocklist(url: &str) -> Option<String> {
    let lower = url.to_lowercase();

    for tld in SUSPICIOUS_TLDS {
        // Extract host from the URL, then strip port (:8080), query (?q=1), and
        // fragment (#anchor) so TLD matching works on the bare hostname.
        let host_raw = lower.split('/').nth(2).unwrap_or("");
        let host = host_raw
            .split([':', '?', '#'])
            .next()
            .unwrap_or(host_raw);
        if host.ends_with(tld) || host.contains(&format!("{tld}/")) {
            return Some(format!("URL uses suspicious TLD: {tld}"));
        }
    }

    for fragment in SUSPICIOUS_PATHS {
        if lower.contains(fragment) {
            return Some(format!("URL contains suspicious path: {fragment}"));
        }
    }

    None
}

/// Queries the VirusTotal v3 API for a URL analysis report.
/// Uses base64url encoding of the URL as required by the VT API.
///
/// Args:
///   url:     The download URL.
///   api_key: VirusTotal API key.
///
/// Returns:
///   Ok(Some(reason)) if malicious > 3 engines, Ok(None) if clean,
///   Err(reason) if VT is unavailable or returns a non-200 status.
async fn check_vt_url(url: &str, api_key: &str) -> Result<Option<String>, String> {
    // VT URL id = base64url(url), no padding.
    let mut encoded = Vec::new();
    base64_url_encode(url.as_bytes(), &mut encoded);
    let id = String::from_utf8_lossy(&encoded).replace('=', "");

    let vt_url = format!("https://www.virustotal.com/api/v3/urls/{id}");
    let client = reqwest::Client::new();
    let resp = client
        .get(&vt_url)
        .header("x-apikey", api_key)
        .send()
        .await
        .map_err(|e| format!("VirusTotal check failed (network error) — result inconclusive: {e}"))?;

    if !resp.status().is_success() {
        let s = resp.status();
        return Err(format!(
            "VirusTotal check failed (HTTP {s}) — result inconclusive"
        ));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("VirusTotal response parse error — result inconclusive: {e}"))?;
    let malicious = json
        .pointer("/data/attributes/last_analysis_stats/malicious")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if malicious > 3 {
        Ok(Some(format!(
            "VirusTotal: {malicious} engine(s) flagged this URL"
        )))
    } else {
        Ok(None)
    }
}

/// Minimal base64url encoder (alphabet A-Z a-z 0-9 - _).
/// Writes output into the provided Vec<u8> and returns the number of bytes written.
///
/// Args:
///   input:  Bytes to encode.
///   output: Vec to write encoded bytes into.
///
/// Returns:
///   Number of bytes written to `output`.
fn base64_url_encode(input: &[u8], output: &mut Vec<u8>) -> usize {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut written = 0;
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let combined = (b0 << 16) | (b1 << 8) | b2;
        output.push(CHARS[((combined >> 18) & 0x3F) as usize]);
        output.push(CHARS[((combined >> 12) & 0x3F) as usize]);
        if chunk.len() > 1 {
            output.push(CHARS[((combined >> 6) & 0x3F) as usize]);
        } else {
            output.push(b'=');
        }
        if chunk.len() > 2 {
            output.push(CHARS[(combined & 0x3F) as usize]);
        } else {
            output.push(b'=');
        }
        written += 4;
    }
    written
}
