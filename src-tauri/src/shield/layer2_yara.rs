use super::{LayerResult, LayerVerdict, ScanContext};

/// Bundled YARA rules loaded from the external file at compile time.
/// The yara-sync GitHub Action updates bundled_rules.yar daily with community rules.
const RULES: &str = include_str!("bundled_rules.yar");

/// Layer 2: YARA rule scan.
/// Compiles the bundled YARA rules and scans the file with a 30-second timeout.
/// Any matching rule is treated as a Threat.
///
/// Args:
///   ctx: Scan context (path is used).
///
/// Returns:
///   LayerResult with Threat if any rule matches, Clean otherwise.
pub async fn scan(ctx: &ScanContext) -> LayerResult {
    let path = ctx.path.clone();

    let result = tokio::task::spawn_blocking(move || -> LayerResult {
        let compiled = match compile_rules() {
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

/// Compiles the bundled YARA rules string into a ready-to-scan Rules object.
///
/// Returns:
///   Compiled yara::Rules on success, or an error string on failure.
fn compile_rules() -> Result<yara::Rules, String> {
    let compiler = yara::Compiler::new().map_err(|e| e.to_string())?;
    let compiler = compiler.add_rules_str(RULES).map_err(|e| e.to_string())?;
    compiler.compile_rules().map_err(|e| e.to_string())
}
