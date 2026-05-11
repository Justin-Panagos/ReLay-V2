pub mod layer1_hash;
pub mod layer2_yara;
pub mod layer3_entropy;
pub mod layer4_filetype;
pub mod layer5_url;
pub mod layer6_heuristics;
pub mod layer7_sandbox;

use std::path::Path;
use std::sync::Mutex;

use crate::db::{self, DbState};
use tauri::Manager;

/// Tauri-managed state: the active YARA rules string loaded from disk at startup.
/// Replacing the string at runtime lets rule updates reach users without a binary release.
pub struct YaraRulesState(pub Mutex<String>);

// ── Types ────────────────────────────────────────────────────────────────────

/// Context passed to every layer during a scan pipeline run.
pub struct ScanContext {
    /// Absolute path to the downloaded file on disk.
    pub path: std::path::PathBuf,
    /// Original download URL (used by Layer 5 URL reputation).
    pub url: String,
    /// File name (last path component).
    pub filename: String,
    /// Hex-encoded SHA-256 digest. Empty until Layer 1 populates it.
    pub sha256: String,
    /// Active YARA rules string, cloned from YaraRulesState at scan time.
    pub yara_rules: String,
}

/// Verdict returned by a single scan layer.
pub enum LayerVerdict {
    /// File passed this layer with no concerns.
    Clean,
    /// File has suspicious characteristics but is not definitively malicious.
    Suspicious {
        #[allow(dead_code)]
        reason: String,
    },
    /// File is a confirmed threat and should be quarantined immediately.
    Threat { reason: String },
}

/// Result produced by a single layer.
pub struct LayerResult {
    /// Layer number (1–6).
    #[allow(dead_code)]
    pub layer: u8,
    /// Human-readable layer name.
    #[allow(dead_code)]
    pub name: &'static str,
    /// The layer's verdict.
    pub verdict: LayerVerdict,
}

/// Final verdict after all layers have run (or the pipeline was cut short).
pub enum FinalVerdict {
    /// File is safe — move to destination and mark complete.
    Clean,
    /// File is a threat — move to quarantine.
    Quarantined {
        /// Human-readable reason for quarantine.
        reason: String,
    },
}

/// Full pipeline result returned by `run_pipeline`.
pub struct PipelineResult {
    /// Hex-encoded SHA-256 of the scanned file.
    pub sha256: String,
    /// Per-layer results (may be fewer than 6 if an early Threat was found).
    #[allow(dead_code)]
    pub layers: Vec<LayerResult>,
    /// Overall verdict.
    pub verdict: FinalVerdict,
    /// JSON-encoded SandboxReport from Layer 7, or None if Layer 7 did not run.
    pub sandbox_report: Option<String>,
}

/// Payload emitted as `download://scan/{db_id}` before each layer starts.
/// Triggers the amber pulse animation on the download card.
#[derive(Clone, serde::Serialize)]
pub struct ScanPayload {
    /// Layer number about to run (0 = pipeline starting).
    pub layer: u8,
    /// Layer name string.
    pub name: String,
}

/// Payload emitted as `download://quarantine/{db_id}` when a file is quarantined.
#[derive(Clone, serde::Serialize)]
pub struct QuarantinePayload {
    /// Human-readable reason the file was quarantined.
    pub reason: String,
    /// Hex-encoded SHA-256 of the quarantined file.
    pub sha256: String,
}

// ── Pipeline ─────────────────────────────────────────────────────────────────

/// Runs the six-layer scan pipeline against a downloaded file.
/// Emits `download://scan/{db_id}` before each layer so the UI can pulse amber.
/// Stops early and returns `FinalVerdict::Quarantined` on the first `Threat`.
///
/// Args:
///   path:     Absolute path to the file on disk.
///   url:      The original download URL.
///   filename: The file name (used in quarantine record).
///   db_id:    Downloads table row id (used in event names and DB writes).
///   app:      Tauri app handle for event emission and DB access.
///
/// Returns:
///   A PipelineResult with sha256, per-layer results, and a FinalVerdict.
pub async fn run_pipeline(
    path: &Path,
    url: &str,
    filename: &str,
    db_id: i64,
    app: &tauri::AppHandle,
) -> PipelineResult {
    // Load the active rules string once per scan. Falls back to the bundled baseline
    // if YaraRulesState was not yet managed (e.g. in tests).
    let yara_rules: String = app
        .try_state::<YaraRulesState>()
        .map(|s| s.0.lock().unwrap_or_else(|p| p.into_inner()).clone())
        .unwrap_or_else(|| layer2_yara::BUNDLED_RULES.to_string());

    let mut ctx = ScanContext {
        path: path.to_path_buf(),
        url: url.to_string(),
        filename: filename.to_string(),
        sha256: String::new(),
        yara_rules,
    };

    let mut layers: Vec<LayerResult> = Vec::with_capacity(7);
    let mut sandbox_report: Option<String> = None;

    // Retrieve VT API key once — passed to layers that need it.
    let vt_key: Option<String> = app
        .try_state::<DbState>()
        .and_then(|db| {
            db.0.lock()
                .ok()
                .and_then(|conn| db::get_setting(&conn, "virustotal_api_key").ok().flatten())
        });

    // Macro: emit scan event, run layer, push result, return early on Threat.
    macro_rules! run_layer {
        ($num:expr, $name:expr, $fut:expr) => {{
            app.emit_all(
                &format!("download://scan/{db_id}"),
                ScanPayload {
                    layer: $num,
                    name: $name.to_string(),
                },
            )
            .ok();
            let result = $fut.await;
            if let LayerVerdict::Threat { ref reason } = result.verdict {
                let reason = reason.clone();
                layers.push(result);
                return PipelineResult {
                    sha256: ctx.sha256.clone(),
                    layers,
                    verdict: FinalVerdict::Quarantined { reason },
                    sandbox_report: None,
                };
            }
            layers.push(result);
        }};
    }

    // Layer 1: Hash — expanded manually so we can hook VT → ICP auto-submit.
    // The macro returns early on Threat, making post-return hooks impossible.
    app.emit_all(
        &format!("download://scan/{db_id}"),
        ScanPayload { layer: 1, name: "Hash".to_string() },
    )
    .ok();
    let l1 = layer1_hash::scan(&mut ctx, vt_key.as_deref(), app).await;
    if let LayerVerdict::Threat { ref reason } = l1.verdict {
        // VT key set → VT (not ICP cache) caught this threat. Layer 1 checks the local
        // ICP cache first and returns early if found, so a Threat verdict here with VT
        // key present means the hash is missing from the canister. Submit it as a
        // community proposal in the background so other clients benefit.
        if vt_key.is_some() && !ctx.sha256.is_empty() {
            let sha256 = ctx.sha256.clone();
            let app_clone = app.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(15),
                    auto_submit_vt_threat(sha256, app_clone),
                )
                .await;
            });
        }
        let reason = reason.clone();
        layers.push(l1);
        return PipelineResult {
            sha256: ctx.sha256.clone(),
            layers,
            verdict: FinalVerdict::Quarantined { reason },
            sandbox_report: None,
        };
    }
    layers.push(l1);

    run_layer!(2, "YARA", layer2_yara::scan(&ctx));
    run_layer!(3, "Entropy", layer3_entropy::scan(&ctx));
    run_layer!(4, "File Type", layer4_filetype::scan(&ctx));
    run_layer!(5, "URL Reputation", layer5_url::scan(&ctx, vt_key.as_deref(), app));
    run_layer!(6, "Heuristics", layer6_heuristics::scan(&ctx));

    // Layer 7: Sandbox — Pro only, opt-in via "sandbox_enabled" = "true" setting.
    let sandbox_enabled = app
        .try_state::<DbState>()
        .and_then(|db| {
            db.0.lock().ok().and_then(|conn| {
                db::get_setting(&conn, "sandbox_enabled").ok().flatten()
            })
        })
        .as_deref()
        == Some("true");

    let pro_ok = app
        .try_state::<crate::pro::LicenceCacheState>()
        .map(|cache| crate::pro::is_pro(&cache))
        .unwrap_or(false);

    if sandbox_enabled && pro_ok {
        app.emit_all(
            &format!("download://scan/{db_id}"),
            ScanPayload {
                layer: 7,
                name: "Sandbox".to_string(),
            },
        )
        .ok();
        let (result, report) = layer7_sandbox::scan(&ctx).await;
        layers.push(result);
        sandbox_report = report.map(|r| serde_json::to_string(&r).unwrap_or_default());
    }

    PipelineResult {
        sha256: ctx.sha256.clone(),
        layers,
        verdict: FinalVerdict::Clean,
        sandbox_report,
    }
}

// ── VT → ICP auto-submit ──────────────────────────────────────────────────────

/// Submits a VirusTotal-detected hash to the ICP governance canister as a
/// community proposal so other clients benefit from the detection.
/// Fire-and-forget — all errors are logged and swallowed; never panics.
///
/// Args:
///   sha256: Hex-encoded SHA-256 of the file VirusTotal flagged.
///   app:    Tauri app handle used to access ConfigState and DbState.
async fn auto_submit_vt_threat(sha256: String, app: tauri::AppHandle) {
    use crate::icp::{agent as icp_agent, ConfigState, DeviceIdentityState};

    let config = match app.try_state::<ConfigState>() {
        Some(c) => c,
        None => return,
    };
    let pem = app
        .try_state::<DeviceIdentityState>()
        .map(|s| s.0.clone())
        .unwrap_or_default();

    let icp = &config.0;
    let a = match icp_agent::build_agent(&icp.icp_url, &pem).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[shield] auto_submit_vt_threat: failed to build IC agent: {e:?}");
            return;
        }
    };
    let governance_id = match icp_agent::parse_principal(&icp.canisters.governance) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[shield] auto_submit_vt_threat: invalid governance principal: {e:?}");
            return;
        }
    };

    match icp_agent::submit_proposal(&a, &governance_id, sha256.clone()).await {
        Ok(proposal_id) => eprintln!(
            "[shield] auto_submit_vt_threat: submitted {sha256} as proposal #{proposal_id}"
        ),
        Err(e) => eprintln!(
            "[shield] auto_submit_vt_threat: ICP submission failed (non-fatal): {e:?}"
        ),
    }
}
