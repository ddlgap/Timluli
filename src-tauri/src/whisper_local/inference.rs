use std::path::Path;
use thiserror::Error;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Whether the Vulkan runtime loader (vulkan-1.dll) can be loaded on this
/// machine. Used (with the `gpu` feature) to avoid faulting on the delay-loaded
/// import when GPU support is compiled in but no Vulkan loader is installed.
#[cfg(all(feature = "gpu", windows))]
fn vulkan_loader_available() -> bool {
    use windows::core::s;
    use windows::Win32::System::LibraryLoader::LoadLibraryA;
    // Safe: loading a system DLL by name; the handle is intentionally leaked
    // (process-lifetime) since whisper.cpp will use the same loader if present.
    unsafe { LoadLibraryA(s!("vulkan-1.dll")).is_ok() }
}

#[cfg(all(feature = "gpu", not(windows)))]
fn vulkan_loader_available() -> bool {
    true
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("שגיאה בטעינת המודל: {0}")]
    Load(String),
    #[error("שגיאה בתמלול: {0}")]
    Transcribe(String),
}

pub struct WhisperEngine {
    ctx: WhisperContext,
    pub model_id: String,
}

// WhisperContext wraps a raw pointer; whisper-rs declares it Send + Sync.
unsafe impl Send for WhisperEngine {}
unsafe impl Sync for WhisperEngine {}

impl WhisperEngine {
    pub fn load(model_path: &Path, model_id: String) -> Result<Self, EngineError> {
        #[allow(unused_mut)]
        let mut cparams = WhisperContextParameters::default();
        // With the `gpu` feature, whisper.cpp defaults to a Vulkan device. If the
        // Vulkan loader (vulkan-1.dll) isn't present, force CPU so we never trigger
        // the delay-loaded import (which would otherwise fault). When the loader is
        // present but exposes no device, whisper.cpp itself falls back to CPU.
        #[cfg(feature = "gpu")]
        if !vulkan_loader_available() {
            cparams.use_gpu(false);
        }
        let ctx = WhisperContext::new_with_params(model_path, cparams)
            .map_err(|e| EngineError::Load(e.to_string()))?;
        Ok(Self { ctx, model_id })
    }

    /// Transcribes 16 kHz mono f32 audio. `lang` must be "he" (ISO-639-1).
    pub fn transcribe(&self, audio_16k_mono: &[f32], lang: &str) -> Result<String, EngineError> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| EngineError::Transcribe(e.to_string()))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(lang));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(num_cpus::get_physical().min(4) as i32);
        params.set_no_context(true);
        params.set_single_segment(true);
        // Disable timestamp tokens entirely — without this, the last segment
        // ends with a single timestamp before EOT, triggering whisper.cpp's
        // "single timestamp ending - skip entire chunk" guard that discards
        // all output. With no_timestamps=true the decoder emits text→EOT
        // directly, bypassing that check.
        params.set_no_timestamps(true);
        params.set_temperature(0.2);
        params.set_temperature_inc(0.0);
        params.set_entropy_thold(2.8);
        params.set_logprob_thold(-1.0);
        params.set_no_speech_thold(0.35);

        state
            .full(params, audio_16k_mono)
            .map_err(|e| EngineError::Transcribe(e.to_string()))?;

        let mut out = String::new();
        for seg in state.as_iter() {
            let text = seg
                .to_str_lossy()
                .map_err(|e| EngineError::Transcribe(e.to_string()))?;
            out.push_str(&text);
        }
        Ok(out.trim().to_string())
    }
}
