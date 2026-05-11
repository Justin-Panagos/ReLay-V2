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

// ── Linux sandbox (unshare-based network namespace isolation) ─────────────────

/// Layer 7: Linux sandbox via `unshare`.
/// Spawns the file inside a new network + user namespace using the `unshare` utility.
/// The child process has no network access; it cannot escalate privileges.
/// Non-executables skip the sandbox and return Clean immediately.
///
/// Args:
///   ctx: Scan context (path is used).
///
/// Returns:
///   A tuple of (LayerResult, Option<SandboxReport>).
#[cfg(target_os = "linux")]
pub async fn scan(ctx: &ScanContext) -> (LayerResult, Option<SandboxReport>) {
    use std::time::Duration;

    if !is_executable(&ctx.path) {
        return (
            LayerResult { layer: 7, name: "Sandbox", verdict: LayerVerdict::Clean },
            None,
        );
    }

    let path_str = ctx.path.to_string_lossy().to_string();

    // unshare --net creates a fresh network namespace with no interfaces.
    // --user creates a new user namespace (prevents privilege escalation).
    // --fork + --pid create a PID namespace so the process can't signal peers.
    let mut child = match tokio::process::Command::new("unshare")
        .args(["--net", "--user", "--fork", "--pid", "--", &path_str])
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[shield] unshare spawn failed: {e}");
            return (
                LayerResult {
                    layer: 7,
                    name: "Sandbox",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("unshare unavailable: {e}"),
                    },
                },
                None,
            );
        }
    };

    // Capture exit status from the first wait; reap after kill on timeout.
    // Never call child.wait() twice — the OS reaps the process on the first call.
    let exit_status = match tokio::time::timeout(Duration::from_secs(45), child.wait()).await {
        Ok(status) => Some(status.ok().map(|s| s.success()).unwrap_or(false)),
        Err(_) => {
            child.kill().await.ok();
            // Reap the killed process to avoid a zombie, then return None (timed out).
            child.wait().await.ok();
            None
        }
    };

    let (verdict_str, notes) = match exit_status {
        None => (
            "suspicious".to_string(),
            "Process still running after 45 s inside Linux namespace sandbox".to_string(),
        ),
        Some(true) => (
            "clean".to_string(),
            "Network isolated via Linux user+net+pid namespace (unshare)".to_string(),
        ),
        Some(false) => (
            "suspicious".to_string(),
            "Process exited with error inside Linux namespace sandbox".to_string(),
        ),
    };

    let layer_verdict = if verdict_str == "suspicious" {
        LayerVerdict::Suspicious { reason: notes.clone() }
    } else {
        LayerVerdict::Clean
    };

    let report = SandboxReport {
        syscalls_blocked: 0,    // namespace isolation; syscall counts require strace/seccomp
        network_attempts: 0,    // network blocked at namespace level, attempts not countable
        file_writes: 0,
        verdict: verdict_str,
        platform: "linux".to_string(),
        notes,
    };

    (
        LayerResult { layer: 7, name: "Sandbox", verdict: layer_verdict },
        Some(report),
    )
}

// ── Windows sandbox (Job Object isolation) ───────────────────────────────────

/// Layer 7: Windows sandbox via Win32 Job Objects.
/// Spawns the file under a Job Object with kill-on-close and unhandled-exception
/// termination limits. Waits up to 45 s for completion.
/// Non-executables skip the sandbox and return Clean immediately.
///
/// Args:
///   ctx: Scan context (path is used).
///
/// Returns:
///   A tuple of (LayerResult, Option<SandboxReport>).
#[cfg(target_os = "windows")]
pub async fn scan(ctx: &ScanContext) -> (LayerResult, Option<SandboxReport>) {
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_TIMEOUT};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    if !is_executable(&ctx.path) {
        return (
            LayerResult { layer: 7, name: "Sandbox", verdict: LayerVerdict::Clean },
            None,
        );
    }

    let path_str = ctx.path.to_string_lossy().to_string();

    // Spawn the target process first so we have a PID to open a handle on.
    let mut child = match tokio::process::Command::new(&path_str).spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                LayerResult {
                    layer: 7,
                    name: "Sandbox",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("spawn failed: {e}"),
                    },
                },
                None,
            );
        }
    };

    let pid = match child.id() {
        Some(p) => p,
        None => {
            // Process exited before we could get the PID.
            return (
                LayerResult { layer: 7, name: "Sandbox", verdict: LayerVerdict::Clean },
                Some(SandboxReport {
                    syscalls_blocked: 0,
                    network_attempts: 0,
                    file_writes: 0,
                    verdict: "clean".to_string(),
                    platform: "windows".to_string(),
                    notes: "Process exited immediately".to_string(),
                }),
            );
        }
    };

    let (verdict_str, notes) = unsafe {
        // Open a handle to the spawned process.
        let proc: HANDLE = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid);
        if proc == 0 {
            ("suspicious".to_string(), "OpenProcess failed — sandbox isolation not applied".to_string())
        } else {
            // Create a Job Object with kill-on-close semantics.
            let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job != 0 {
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                    BasicLimitInformation: std::mem::zeroed(),
                    IoInfo: std::mem::zeroed(),
                    ProcessMemoryLimit: 0,
                    JobMemoryLimit: 0,
                    PeakProcessMemoryUsed: 0,
                    PeakJobMemoryUsed: 0,
                };
                limits.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;

                // Both Win32 calls return BOOL (0 = failure). Check both — if either fails
                // the process is not contained, so we must not report Clean.
                let setup_ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0 && AssignProcessToJobObject(job, proc) != 0;

                if !setup_ok {
                    CloseHandle(job);
                    CloseHandle(proc);
                    child.kill().await.ok();
                    child.wait().await.ok();
                    return (
                        LayerResult {
                            layer: 7,
                            name: "Sandbox",
                            verdict: LayerVerdict::Suspicious {
                                reason: "Job Object sandbox setup failed — process not contained"
                                    .to_string(),
                            },
                        },
                        None,
                    );
                }
            }

            // Wait up to 45 s.
            let wait = WaitForSingleObject(proc, 45_000);
            let timed_out = wait == WAIT_TIMEOUT;
            let wait_failed = wait == WAIT_FAILED;

            if timed_out {
                child.kill().await.ok();
            }

            if job != 0 {
                CloseHandle(job);
            }
            CloseHandle(proc);

            if timed_out {
                ("suspicious".to_string(), "Process still running after 45 s in Job Object sandbox".to_string())
            } else if wait_failed {
                ("suspicious".to_string(), "WaitForSingleObject failed — sandbox result uncertain".to_string())
            } else {
                let exit_ok = child.wait().await.ok().map(|s| s.success()).unwrap_or(false);
                if exit_ok {
                    ("clean".to_string(), "Process completed normally under Job Object sandbox".to_string())
                } else {
                    ("suspicious".to_string(), "Process exited with error under Job Object sandbox".to_string())
                }
            }
        }
    };

    let layer_verdict = if verdict_str == "suspicious" {
        LayerVerdict::Suspicious { reason: notes.clone() }
    } else {
        LayerVerdict::Clean
    };

    let report = SandboxReport {
        syscalls_blocked: 0,
        network_attempts: 0,
        file_writes: 0,
        verdict: verdict_str,
        platform: "windows".to_string(),
        notes,
    };

    (
        LayerResult { layer: 7, name: "Sandbox", verdict: layer_verdict },
        Some(report),
    )
}

// ── Fallback stub for unsupported platforms ───────────────────────────────────

/// Stub for platforms with no sandbox implementation.
/// Returns Suspicious so the UI shows the layer was skipped rather than silently passing.
///
/// Args:
///   _ctx: Scan context (unused).
///
/// Returns:
///   (LayerResult with Suspicious verdict, Some(SandboxReport with platform="unsupported")).
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub async fn scan(_ctx: &ScanContext) -> (LayerResult, Option<SandboxReport>) {
    let report = SandboxReport {
        syscalls_blocked: 0,
        network_attempts: 0,
        file_writes: 0,
        verdict: "unsupported".to_string(),
        platform: "unsupported".to_string(),
        notes: "Sandbox scanning is not available on this platform.".to_string(),
    };
    (
        LayerResult {
            layer: 7,
            name: "Sandbox",
            verdict: LayerVerdict::Suspicious {
                reason: "Sandbox layer skipped (unsupported platform)".to_string(),
            },
        },
        Some(report),
    )
}
