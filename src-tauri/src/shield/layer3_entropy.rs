use memmap2::Mmap;
use std::fs::File;

use super::{LayerResult, LayerVerdict, ScanContext};

/// Files smaller than this are skipped (too small for meaningful entropy analysis).
const MIN_FILE_SIZE: u64 = 4096;

/// Shannon entropy threshold above which a file is flagged as Suspicious.
/// Fully encrypted or compressed data typically scores 7.5–8.0.
const ENTROPY_THRESHOLD: f64 = 7.2;

/// Layer 3: Shannon entropy check.
/// Memory-maps the file and computes H = -Σ p(x)*log2(p(x)) over all 256 byte values.
/// Files with entropy above `ENTROPY_THRESHOLD` are flagged as Suspicious (not Threat —
/// high entropy alone is only a heuristic; compressed archives score similarly).
///
/// Args:
///   ctx: Scan context (path is used).
///
/// Returns:
///   LayerResult with Suspicious if H > 7.2, Clean otherwise.
pub async fn scan(ctx: &ScanContext) -> LayerResult {
    let path = ctx.path.clone();

    let result = tokio::task::spawn_blocking(move || -> LayerResult {
        // Skip very small files.
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                return LayerResult {
                    layer: 3,
                    name: "Entropy",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("could not stat file: {e}"),
                    },
                };
            }
        };

        if meta.len() < MIN_FILE_SIZE {
            return LayerResult {
                layer: 3,
                name: "Entropy",
                verdict: LayerVerdict::Clean,
            };
        }

        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                return LayerResult {
                    layer: 3,
                    name: "Entropy",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("could not open file: {e}"),
                    },
                };
            }
        };

        // SAFETY: we do not mutate the mapping and the file is not modified during scan.
        let mmap = match unsafe { Mmap::map(&file) } {
            Ok(m) => m,
            Err(e) => {
                return LayerResult {
                    layer: 3,
                    name: "Entropy",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("mmap failed: {e}"),
                    },
                };
            }
        };

        let entropy = shannon_entropy(&mmap[..]);

        if entropy > ENTROPY_THRESHOLD {
            LayerResult {
                layer: 3,
                name: "Entropy",
                verdict: LayerVerdict::Suspicious {
                    reason: format!("high Shannon entropy ({entropy:.2}) — possible packed/encrypted content"),
                },
            }
        } else {
            LayerResult {
                layer: 3,
                name: "Entropy",
                verdict: LayerVerdict::Clean,
            }
        }
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => LayerResult {
            layer: 3,
            name: "Entropy",
            verdict: LayerVerdict::Suspicious {
                reason: "entropy task panicked".to_string(),
            },
        },
    }
}

/// Computes the Shannon entropy of a byte slice.
/// H = -Σ_{x=0}^{255} p(x) * log2(p(x))  where p(x) = freq[x] / len.
///
/// Args:
///   data: Byte slice to analyse.
///
/// Returns:
///   Entropy value in bits per byte (0.0 – 8.0).
fn shannon_entropy(data: &[u8]) -> f64 {
    let len = data.len();
    if len == 0 {
        return 0.0;
    }
    let mut freq = [0u64; 256];
    for &b in data {
        freq[b as usize] += 1;
    }
    let len_f = len as f64;
    freq.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len_f;
            -p * p.log2()
        })
        .sum()
}
