use super::{LayerResult, LayerVerdict, ScanContext};

/// Baseline YARA rules compiled into the binary. Copied to the app data directory
/// on first launch so the file on disk can be updated independently of app releases.
pub const BUNDLED_RULES: &str = include_str!("bundled_rules.yar");

/// Layer 2: YARA rule scan.
/// Compiles the rules string from the scan context and scans the file with a 30-second timeout.
/// Any matching rule is treated as a Threat.
///
/// Args:
///   ctx: Scan context (path and yara_rules are used).
///
/// Returns:
///   LayerResult with Threat if any rule matches, Clean otherwise.
pub async fn scan(ctx: &ScanContext) -> LayerResult {
    let path = ctx.path.clone();
    let rules = ctx.yara_rules.clone();

    let result = tokio::task::spawn_blocking(move || -> LayerResult {
        let compiled = match compile_rules(&rules) {
            Ok(r) => r,
            Err(e) => {
                return LayerResult {
                    layer: 2,
                    name: "YARA",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("YARA compile error: {e}"),
                    },
                };
            }
        };

        match compiled.scan_file(&path, 30) {
            Ok(matches) if !matches.is_empty() => {
                let names: Vec<_> = matches.iter().map(|m| m.identifier).collect();
                LayerResult {
                    layer: 2,
                    name: "YARA",
                    verdict: LayerVerdict::Threat {
                        reason: format!("YARA rule match: {}", names.join(", ")),
                    },
                }
            }
            Ok(_) => LayerResult {
                layer: 2,
                name: "YARA",
                verdict: LayerVerdict::Clean,
            },
            Err(e) => LayerResult {
                layer: 2,
                name: "YARA",
                verdict: LayerVerdict::Suspicious {
                    reason: format!("YARA scan error: {e}"),
                },
            },
        }
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => LayerResult {
            layer: 2,
            name: "YARA",
            verdict: LayerVerdict::Suspicious {
                reason: "YARA task panicked".to_string(),
            },
        },
    }
}

/// Compiles a YARA rules string into a ready-to-scan Rules object.
///
/// Args:
///   rules: The YARA rules source text to compile.
///
/// Returns:
///   Compiled yara::Rules on success, or an error string on failure.
fn compile_rules(rules: &str) -> Result<yara::Rules, String> {
    let compiler = yara::Compiler::new().map_err(|e| e.to_string())?;
    let compiler = compiler.add_rules_str(rules).map_err(|e| e.to_string())?;
    compiler.compile_rules().map_err(|e| e.to_string())
}
