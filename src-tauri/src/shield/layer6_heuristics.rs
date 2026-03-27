use goblin::Object;

use super::{LayerResult, LayerVerdict, ScanContext};

/// PE imports that are commonly used by malware for process injection and hollowing.
const SUSPICIOUS_PE_IMPORTS: &[&str] = &[
    "VirtualAlloc",
    "VirtualAllocEx",
    "WriteProcessMemory",
    "CreateRemoteThread",
    "CreateRemoteThreadEx",
    "NtUnmapViewOfSection",
    "SetThreadContext",
    "QueueUserAPC",
    "RtlCreateUserThread",
];

/// Layer 6: Binary header heuristics via goblin.
/// Parses PE (Windows) and ELF (Linux) headers looking for suspicious characteristics.
/// Non-binary files are skipped silently (Clean).
///
/// Checks:
///   PE: presence of known process-injection imports → Suspicious
///   ELF: PT_GNU_STACK segment with execute permission set → Suspicious
///
/// Args:
///   ctx: Scan context (path is used).
///
/// Returns:
///   LayerResult with Suspicious or Clean.
pub async fn scan(ctx: &ScanContext) -> LayerResult {
    let path = ctx.path.clone();

    let result = tokio::task::spawn_blocking(move || -> LayerResult {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                return LayerResult {
                    layer: 6,
                    name: "Heuristics",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("could not read file for heuristic scan: {e}"),
                    },
                };
            }
        };

        match Object::parse(&bytes) {
            Ok(Object::PE(pe)) => check_pe(&pe),
            Ok(Object::Elf(elf)) => check_elf(&elf),
            Ok(_) => LayerResult {
                layer: 6,
                name: "Heuristics",
                verdict: LayerVerdict::Clean,
            },
            Err(_) => {
                // Not a recognised binary — skip.
                LayerResult {
                    layer: 6,
                    name: "Heuristics",
                    verdict: LayerVerdict::Clean,
                }
            }
        }
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => LayerResult {
            layer: 6,
            name: "Heuristics",
            verdict: LayerVerdict::Suspicious {
                reason: "heuristics task panicked".to_string(),
            },
        },
    }
}

/// Inspects a parsed PE binary for suspicious imports.
///
/// Args:
///   pe: Parsed goblin PE object.
///
/// Returns:
///   LayerResult with Suspicious if injection-related imports found, Clean otherwise.
fn check_pe(pe: &goblin::pe::PE) -> LayerResult {
    let mut flagged: Vec<String> = Vec::new();

    for import in &pe.imports {
        let name: &str = import.name.as_ref();
        if SUSPICIOUS_PE_IMPORTS.contains(&name) {
            flagged.push(name.to_string());
        }
    }

    if !flagged.is_empty() {
        LayerResult {
            layer: 6,
            name: "Heuristics",
            verdict: LayerVerdict::Suspicious {
                reason: format!(
                    "PE imports suggest process injection: {}",
                    flagged.join(", ")
                ),
            },
        }
    } else {
        LayerResult {
            layer: 6,
            name: "Heuristics",
            verdict: LayerVerdict::Clean,
        }
    }
}

/// Inspects a parsed ELF binary for executable stack segments.
///
/// Args:
///   elf: Parsed goblin ELF object.
///
/// Returns:
///   LayerResult with Suspicious if PT_GNU_STACK has execute permission, Clean otherwise.
fn check_elf(elf: &goblin::elf::Elf) -> LayerResult {
    use goblin::elf::program_header::{PF_X, PT_GNU_STACK};

    for ph in &elf.program_headers {
        if ph.p_type == PT_GNU_STACK && (ph.p_flags & PF_X) != 0 {
            return LayerResult {
                layer: 6,
                name: "Heuristics",
                verdict: LayerVerdict::Suspicious {
                    reason: "ELF has executable stack (PT_GNU_STACK with PF_X)".to_string(),
                },
            };
        }
    }

    LayerResult {
        layer: 6,
        name: "Heuristics",
        verdict: LayerVerdict::Clean,
    }
}
