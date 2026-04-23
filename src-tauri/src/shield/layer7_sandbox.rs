use super::{LayerResult, LayerVerdict, ScanContext};

/// MIME types that indicate an executable file worth sandboxing.
const EXEC_MIMES: &[&str] = &[
    "application/x-dosexec",
    "application/x-executable",
    "application/x-mach-binary",
    "application/x-sharedlib",
];

/// Structured report produced by the macOS sandbox run.
/// Serialised to JSON and stored in the `sandbox_report` DB column.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SandboxReport {
    /// Number of syscalls blocked by the sandbox profile.
    pub syscalls_blocked: u32,
    /// Number of outbound network connection attempts denied.
    pub network_attempts: u32,
    /// Number of file write attempts denied.
    pub file_writes: u32,
    /// Overall sandbox verdict: "clean" | "suspicious".
    pub verdict: String,
    /// Platform string: "macos" | "unsupported".
    pub platform: String,
    /// Human-readable note appended to the verdict.
    pub notes: String,
}

/// Layer 7: macOS sandbox execution.
/// Runs the file inside `sandbox-exec` with a restrictive SBPL profile for up to 45 s.
/// Queries the unified log for sandbox violations and builds a `SandboxReport`.
/// Non-executables skip the sandbox and return Clean immediately.
///
/// Args:
///   ctx: Scan context (path is used).
///
/// Returns:
///   A tuple of (LayerResult, Option<SandboxReport>).
///   The report is Some when the sandbox actually ran, None when skipped.
#[cfg(target_os = "macos")]
pub async fn scan(ctx: &ScanContext) -> (LayerResult, Option<SandboxReport>) {
    use std::time::Duration;

    // Only sandbox recognised executables.
    if !is_executable(&ctx.path) {
        return (
            LayerResult {
                layer: 7,
                name: "Sandbox",
                verdict: LayerVerdict::Clean,
            },
            None,
        );
    }

    let path_str = ctx.path.to_string_lossy().to_string();

    // Restrictive SBPL profile: deny everything except reading and executing self.
    let profile = "(version 1)\n\
                   (deny default)\n\
                   (allow process-exec)\n\
                   (allow file-read*)\n\
                   (deny file-write*)\n\
                   (deny network*)";

    // Spawn under sandbox-exec with a 45-second hard timeout.
    // Use spawn() + wait() so we hold the child handle and can kill it on timeout.
    // Safety: args() calls execv() directly — no shell interpolation occurs,
    // so metacharacters in path_str are inert.
    let mut child = match tokio::process::Command::new("sandbox-exec")
        .args(["-p", profile, &path_str])
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[shield] sandbox-exec spawn failed: {e}");
            return (
                LayerResult {
                    layer: 7,
                    name: "Sandbox",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("sandbox-exec unavailable: {e}"),
                    },
                },
                None,
            );
        }
    };

    match tokio::time::timeout(Duration::from_secs(45), child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            // Timed out — kill the process so it doesn't linger.
            child.kill().await.ok();
        }
    }

    // Query the unified log for sandbox denials in the last 2 minutes.
    let (syscalls_blocked, network_attempts, file_writes) = query_log_violations().await;

    let (verdict_str, notes) = if network_attempts > 0 {
        (
            "suspicious".to_string(),
            format!("File attempted {network_attempts} outbound network connection(s)"),
        )
    } else if file_writes > 5 {
        (
            "suspicious".to_string(),
            format!("File attempted {file_writes} file write(s)"),
        )
    } else if syscalls_blocked > 50 {
        (
            "suspicious".to_string(),
            format!("{syscalls_blocked} syscall(s) blocked — possible evasion"),
        )
    } else {
        ("clean".to_string(), String::new())
    };

    let layer_verdict = if verdict_str == "suspicious" {
        LayerVerdict::Suspicious {
            reason: notes.clone(),
        }
    } else {
        LayerVerdict::Clean
    };

    let report = SandboxReport {
        syscalls_blocked,
        network_attempts,
        file_writes,
        verdict: verdict_str,
        platform: "macos".to_string(),
        notes,
    };

    (
        LayerResult {
            layer: 7,
            name: "Sandbox",
            verdict: layer_verdict,
        },
        Some(report),
    )
}

/// Queries the macOS unified log for sandbox denial entries in the last 2 minutes.
/// Counts network denials, file-write denials, and all other denials separately.
///
/// Returns:
///   (syscalls_blocked, network_attempts, file_writes)
#[cfg(target_os = "macos")]
async fn query_log_violations() -> (u32, u32, u32) {
    let output = tokio::process::Command::new("log")
        .args([
            "show",
            "--predicate",
            "category == \"sandbox\"",
            "--last",
            "2m",
            "--style",
            "json",
        ])
        .output()
        .await;

    let Ok(out) = output else {
        return (0, 0, 0);
    };

    let text = String::from_utf8_lossy(&out.stdout);

    // The log output is a JSON array of objects with a "composedMessage" field.
    let Ok(entries) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (0, 0, 0);
    };

    let Some(arr) = entries.as_array() else {
        return (0, 0, 0);
    };

    let mut network_attempts: u32 = 0;
    let mut file_writes: u32 = 0;
    let mut other_denials: u32 = 0;

    for entry in arr {
        let msg = entry
            .get("composedMessage")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        if msg.contains("deny") {
            if msg.contains("network") {
                network_attempts += 1;
            } else if msg.contains("file-write") {
                file_writes += 1;
            } else {
                other_denials += 1;
            }
        }
    }

    (
        network_attempts + file_writes + other_denials,
        network_attempts,
        file_writes,
    )
}

/// Returns true if `infer` recognises the file as an executable MIME type.
///
/// Args:
///   path: Path to the file to inspect.
///
/// Returns:
///   true if the file appears to be an executable, false otherwise.
fn is_executable(path: &std::path::Path) -> bool {
    if let Ok(Some(kind)) = infer::get_from_path(path) {
        return EXEC_MIMES.contains(&kind.mime_type());
    }
    false
}

/// Stub for non-macOS platforms — sandbox scanning requires sandbox-exec (macOS only).
/// Returns a Suspicious verdict with an "unsupported" note so the scan log shows the
/// layer was skipped rather than silently passing, which would be a false Clean signal.
///
/// Args:
///   _ctx: Scan context (unused on non-macOS).
///
/// Returns:
///   (LayerResult with Suspicious verdict, Some(SandboxReport with platform="unsupported")).
#[cfg(not(target_os = "macos"))]
pub async fn scan(_ctx: &ScanContext) -> (LayerResult, Option<SandboxReport>) {
    let report = SandboxReport {
        syscalls_blocked: 0,
        network_attempts: 0,
        file_writes: 0,
        verdict: "unsupported".to_string(),
        platform: "unsupported".to_string(),
        notes: "Sandbox scanning requires macOS sandbox-exec — skipped on this platform.".to_string(),
    };
    (
        LayerResult {
            layer: 7,
            name: "Sandbox",
            verdict: LayerVerdict::Suspicious {
                reason: "Sandbox layer skipped (non-macOS platform)".to_string(),
            },
        },
        Some(report),
    )
}
