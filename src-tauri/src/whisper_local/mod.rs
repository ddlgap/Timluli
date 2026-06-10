pub mod audio;
pub mod inference;

use inference::{EngineError, Segment, WhisperEngine};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Thread-safe handle that owns a loaded WhisperEngine.
/// `parking_lot::Mutex<Option<Arc<LocalEngineHandle>>>` in AppState controls
/// whether a model is loaded; the inner `tokio::sync::Mutex` serializes
/// concurrent transcribe calls (WhisperState must not be shared).
pub struct LocalEngineHandle {
    pub model_id: String,
    engine: Arc<Mutex<WhisperEngine>>,
}

impl LocalEngineHandle {
    pub fn new(engine: WhisperEngine) -> Self {
        Self {
            model_id: engine.model_id.clone(),
            engine: Arc::new(Mutex::new(engine)),
        }
    }

    /// Runs transcription in a blocking thread while holding the inference lock.
    /// The `lang` parameter must be "he" for the ivrit.ai models.
    pub async fn transcribe(
        &self,
        samples: Vec<f32>,
        lang: &'static str,
    ) -> Result<String, EngineError> {
        // Acquire an owned guard so it can be moved into spawn_blocking.
        let guard = Arc::clone(&self.engine).lock_owned().await;
        tokio::task::spawn_blocking(move || guard.transcribe(&samples, lang))
            .await
            .map_err(|e| EngineError::Transcribe(e.to_string()))?
    }

    /// Like [`Self::transcribe`], but returns timed [`Segment`]s for the video→SRT
    /// pipeline. Serializes on the same inference lock; `lang` must be "he".
    pub async fn transcribe_segments(
        &self,
        samples: Vec<f32>,
        lang: &'static str,
    ) -> Result<Vec<Segment>, EngineError> {
        let guard = Arc::clone(&self.engine).lock_owned().await;
        tokio::task::spawn_blocking(move || guard.transcribe_segments(&samples, lang))
            .await
            .map_err(|e| EngineError::Transcribe(e.to_string()))?
    }

    /// Like [`Self::transcribe_segments`], but also returns word-level timings
    /// (chunk-relative) for the karaoke `words.json` sidecar. Same inference lock.
    pub async fn transcribe_segments_words(
        &self,
        samples: Vec<f32>,
        lang: &'static str,
    ) -> Result<(Vec<Segment>, Vec<inference::WordSpan>), EngineError> {
        let guard = Arc::clone(&self.engine).lock_owned().await;
        tokio::task::spawn_blocking(move || guard.transcribe_segments_words(&samples, lang))
            .await
            .map_err(|e| EngineError::Transcribe(e.to_string()))?
    }
}
