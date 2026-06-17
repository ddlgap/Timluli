//! Shared ONNX Runtime initialization. Several in-process features load models via
//! `ort` (load-dynamic): the Hebrew punctuation engine and the acoustic gender
//! classifier. `ort` must register the bundled app-local `onnxruntime.dll` EXACTLY
//! ONCE per process — this module owns that single init so the features coexist
//! without a double-`commit` (which `ort` rejects). The DLL is resolved next to the
//! executable, where it is bundled app-locally like the vcruntime DLLs.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Resolve `onnxruntime.dll` next to the exe and register it with `ort` exactly once.
/// Idempotent and thread-safe — every ONNX feature calls this before building a
/// `Session`; only the first call does the work, the rest get the cached result.
pub fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| match resolve_dll() {
        Some(p) => ort::init_from(p.to_string_lossy().to_string())
            .commit()
            .map(|_| ())
            .map_err(|e| format!("אתחול ONNX Runtime נכשל: {e}")),
        None => Err("onnxruntime.dll לא נמצא ליד קובץ ההפעלה".to_string()),
    })
    .clone()
}

fn resolve_dll() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("onnxruntime.dll"));
            candidates.push(dir.join("onnxruntime").join("onnxruntime.dll"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("onnxruntime.dll"));
    }
    candidates.into_iter().find(|p| p.exists())
}
