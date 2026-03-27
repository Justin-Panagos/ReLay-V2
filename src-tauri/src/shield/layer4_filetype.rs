use super::{LayerResult, LayerVerdict, ScanContext};

/// MIME types that represent executable or shellcode content.
const EXEC_MIMES: &[&str] = &[
    "application/x-dosexec",      // PE / Windows EXE/DLL
    "application/x-executable",   // ELF
    "application/x-sharedlib",    // shared library
    "application/x-mach-binary",  // Mach-O
    "application/x-msdownload",   // Windows installer / DLL
];

/// Document-like extensions that should never be an executable binary.
const DOCUMENT_EXTS: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
    "txt", "csv", "rtf", "odt", "ods", "odp",
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg",
    "mp3", "mp4", "avi", "mov", "mkv", "wav", "flac",
    "zip", "rar", "7z", "tar", "gz",
];

/// Layer 4: File-type mismatch check.
/// Uses the `infer` crate to detect the real MIME type from magic bytes,
/// then compares it against the declared file extension.
/// An executable binary disguised as a document extension is a Threat.
/// Other mismatches (e.g. wrong archive type) are Suspicious.
///
/// Args:
///   ctx: Scan context (path and filename are used).
///
/// Returns:
///   LayerResult with Threat, Suspicious, or Clean.
pub async fn scan(ctx: &ScanContext) -> LayerResult {
    let path = ctx.path.clone();
    let filename = ctx.filename.clone();

    let result = tokio::task::spawn_blocking(move || -> LayerResult {
        let detected_mime = match infer::get_from_path(&path) {
            Ok(Some(kind)) => kind.mime_type().to_string(),
            Ok(None) => {
                // Unknown type — can't make a determination
                return LayerResult {
                    layer: 4,
                    name: "File Type",
                    verdict: LayerVerdict::Clean,
                };
            }
            Err(e) => {
                return LayerResult {
                    layer: 4,
                    name: "File Type",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!("could not infer file type: {e}"),
                    },
                };
            }
        };

        let declared_ext = std::path::Path::new(&filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let is_exec_content = EXEC_MIMES.contains(&detected_mime.as_str());
        let is_doc_ext = DOCUMENT_EXTS.contains(&declared_ext.as_str());

        if is_exec_content && is_doc_ext {
            return LayerResult {
                layer: 4,
                name: "File Type",
                verdict: LayerVerdict::Threat {
                    reason: format!(
                        "executable content ({detected_mime}) disguised as .{declared_ext}"
                    ),
                },
            };
        }

        // Benign mismatch (e.g. .jpg that is actually a PNG) — flag as Suspicious.
        // Both must be known types for this to fire.
        let ext_mime = mime_from_ext(&declared_ext);
        if let Some(expected) = ext_mime {
            if expected != detected_mime && !declared_ext.is_empty() {
                return LayerResult {
                    layer: 4,
                    name: "File Type",
                    verdict: LayerVerdict::Suspicious {
                        reason: format!(
                            "file extension .{declared_ext} does not match content type {detected_mime}"
                        ),
                    },
                };
            }
        }

        LayerResult {
            layer: 4,
            name: "File Type",
            verdict: LayerVerdict::Clean,
        }
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => LayerResult {
            layer: 4,
            name: "File Type",
            verdict: LayerVerdict::Suspicious {
                reason: "file-type task panicked".to_string(),
            },
        },
    }
}

/// Returns the canonical MIME type for a known file extension, or None.
///
/// Args:
///   ext: Lowercase file extension without leading dot.
///
/// Returns:
///   Some(&str) MIME type, or None if unknown.
fn mime_from_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf"  => "application/pdf",
        "jpg" | "jpeg" => "image/jpeg",
        "png"  => "image/png",
        "gif"  => "image/gif",
        "mp3"  => "audio/mpeg",
        "mp4"  => "video/mp4",
        "zip"  => "application/zip",
        "gz"   => "application/gzip",
        "tar"  => "application/x-tar",
        _      => return None,
    })
}
