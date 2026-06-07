//! Hebrew auto-punctuation engine handle. A `parking_lot::Mutex<Option<Arc<...>>>`
//! in `AppState` controls whether the model is loaded (None = off / not installed).
//! The inner `parking_lot::Mutex<PunctEngine>` serializes inference. Inference is
//! CPU-bound (~26 ms); a `parking_lot` (not `tokio`) inner mutex lets BOTH callers
//! use it: the local engine (async, via `spawn_blocking`) and the online Chrome
//! sidecar (its plain HTTP thread, synchronously) — neither needs to hold a lock
//! across `.await`.

pub mod engine;

use std::path::Path;
use std::sync::Arc;

use engine::PunctEngine;
use parking_lot::Mutex;

pub struct PunctuationEngineHandle {
    engine: Arc<Mutex<PunctEngine>>,
}

impl PunctuationEngineHandle {
    pub fn new(engine: PunctEngine) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
        }
    }

    /// Loads the model + tokenizer (blocking — call inside `spawn_blocking`).
    pub fn load(model_path: &Path, tokenizer_path: &Path) -> Result<Self, String> {
        Ok(Self::new(PunctEngine::load(model_path, tokenizer_path)?))
    }

    /// Synchronous punctuate — for callers already off the async runtime (the
    /// Chrome-sidecar HTTP thread). Blocks ~26 ms. Returns original text on failure.
    pub fn punctuate_blocking(&self, text: &str, ensure_terminal: bool) -> String {
        self.engine.lock().punctuate(text, ensure_terminal)
    }

    /// Async punctuate — runs the blocking inference on `spawn_blocking` so it never
    /// stalls the async runtime. Used by the local-engine path.
    pub async fn punctuate(&self, text: String, ensure_terminal: bool) -> String {
        let engine = Arc::clone(&self.engine);
        let fallback = text.clone();
        tokio::task::spawn_blocking(move || engine.lock().punctuate(&text, ensure_terminal))
            .await
            .unwrap_or(fallback)
    }
}
