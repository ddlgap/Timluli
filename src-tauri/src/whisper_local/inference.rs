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

/// 0.5 s of 16 kHz silence. Appended before timestamped inference so whisper.cpp's
/// "single timestamp ending – skip entire chunk" guard does not discard the chunk
/// (mirrors the live/audio-file paths, which pad before calling `transcribe`).
const PAD_SAMPLES: usize = 16_000 / 2;

/// A transcribed segment with its time bounds in **centiseconds** (1/100 s) — the
/// native unit whisper.cpp reports (`whisper_full_get_segment_t0/t1`). Produced by
/// [`WhisperEngine::transcribe_segments`] for the video→SRT pipeline; the plain
/// `transcribe` path discards timestamps and never builds these.
#[derive(Debug, Clone)]
pub struct Segment {
    pub start_cs: i64,
    pub end_cs: i64,
    pub text: String,
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

    /// Like [`Self::transcribe`], but returns per-segment text **with timestamps**
    /// (centiseconds) instead of one concatenated string. Backs the video→SRT
    /// pipeline. `lang` must be "he" (ISO-639-1) for the ivrit.ai models.
    ///
    /// Two `FullParams` flags differ from `transcribe`: timestamps are **enabled**
    /// (`no_timestamps=false`) and segmentation is **multi-segment**
    /// (`single_segment=false`), so whisper emits a timed `start`/`end` per
    /// utterance instead of one 30 s window. Everything else (language, sampling,
    /// thresholds, threads) is identical, so transcription quality matches the
    /// plain path.
    ///
    /// The audio is padded with 0.5 s of trailing silence **inside** this method so
    /// the final segment closes on a paired timestamp and whisper.cpp does not
    /// discard the chunk via its "single timestamp ending – skip entire chunk"
    /// guard. Callers therefore pass raw (un-padded) chunks — unlike the plain path,
    /// where the caller pads.
    pub fn transcribe_segments(
        &self,
        audio_16k_mono: &[f32],
        lang: &str,
    ) -> Result<Vec<Segment>, EngineError> {
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
        // The two flags that differ from `transcribe`: keep per-utterance segments
        // and emit their timestamps (t0/t1) rather than suppressing them.
        params.set_single_segment(false);
        params.set_no_timestamps(false);
        params.set_temperature(0.2);
        params.set_temperature_inc(0.0);
        params.set_entropy_thold(2.8);
        params.set_logprob_thold(-1.0);
        params.set_no_speech_thold(0.35);

        // Trailing silence pad (see doc comment) — satisfies the skip guard now that
        // timestamps are on.
        let mut padded = Vec::with_capacity(audio_16k_mono.len() + PAD_SAMPLES);
        padded.extend_from_slice(audio_16k_mono);
        padded.extend(std::iter::repeat(0.0_f32).take(PAD_SAMPLES));

        state
            .full(params, &padded)
            .map_err(|e| EngineError::Transcribe(e.to_string()))?;

        let mut segments = Vec::new();
        for seg in state.as_iter() {
            let text = seg
                .to_str_lossy()
                .map_err(|e| EngineError::Transcribe(e.to_string()))?
                .trim()
                .to_string();
            // Skip empties (e.g. a segment whisper opens over the trailing pad).
            if text.is_empty() {
                continue;
            }
            segments.push(Segment {
                start_cs: seg.start_timestamp(),
                end_cs: seg.end_timestamp(),
                text,
            });
        }
        Ok(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::path::PathBuf;

    /// Decodes any symphonia-supported audio/video file to 16 kHz mono f32, reusing
    /// the crate's downmix/resample helpers (mirrors `transcription::local::decode_audio`,
    /// which is private to that module).
    fn decode_to_16k_mono(path: &Path) -> Vec<f32> {
        use symphonia::core::audio::SampleBuffer;
        use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
        use symphonia::core::errors::Error as SymphoniaError;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let file = std::fs::File::open(path).expect("open audio file");
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .expect("probe audio format");
        let mut format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .expect("audio track");
        let track_id = track.id;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(16_000);
        let mut channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(1);
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .expect("make decoder");

        let mut samples: Vec<f32> = Vec::new();
        while let Ok(packet) = format.next_packet() {
            if packet.track_id() != track_id {
                continue;
            }
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    channels = spec.channels.count().max(1);
                    let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                    buf.copy_interleaved_ref(decoded);
                    samples.extend_from_slice(buf.samples());
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => break,
            }
        }
        let mono = crate::whisper_local::audio::downmix_to_mono(&samples, channels);
        crate::whisper_local::audio::resample_to_16k(&mono, sample_rate).expect("resample to 16k")
    }

    /// Behavioral validation for [`WhisperEngine::transcribe_segments`] against a
    /// real installed model and a real speech clip. Confirms the core risk is gone:
    /// with timestamps enabled, whisper.cpp returns timed segments and does NOT
    /// discard the chunk via its "single timestamp ending – skip" guard.
    ///
    /// `#[ignore]`d — it needs external files (no ffmpeg required; symphonia decodes
    /// the input). To run:
    ///   $env:TIMLULI_TEST_MODEL = "$env:APPDATA\studio.oliel.timluli\models\<id>\ggml-model.bin"
    ///   $env:TIMLULI_TEST_AUDIO = "C:\path\to\any-hebrew-clip.mp3"   # or .mp4/.wav/.m4a…
    ///   cargo test --manifest-path src-tauri/Cargo.toml transcribe_segments_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn transcribe_segments_smoke() {
        let model = std::env::var("TIMLULI_TEST_MODEL")
            .expect("set TIMLULI_TEST_MODEL to an installed GGML model file path");
        let audio_path = std::env::var("TIMLULI_TEST_AUDIO")
            .expect("set TIMLULI_TEST_AUDIO to a speech clip (mp3/mp4/wav/m4a…)");

        let mut audio = decode_to_16k_mono(&PathBuf::from(&audio_path));
        assert!(!audio.is_empty(), "decoded to zero samples");
        // Exercise exactly one production-sized window (~28 s); the production path
        // splits long audio into chunks of this size before calling.
        let cap = 28 * 16_000;
        if audio.len() > cap {
            audio.truncate(cap);
        }
        println!(
            "\n=== feeding {:.1}s of 16kHz mono audio ===",
            audio.len() as f32 / 16_000.0
        );

        let engine =
            WhisperEngine::load(&PathBuf::from(&model), "test".into()).expect("load model");
        let segs = engine
            .transcribe_segments(&audio, "he")
            .expect("transcribe_segments returned an error");

        println!("=== transcribe_segments: {} segment(s) ===", segs.len());
        for s in &segs {
            println!(
                "  [{:>3}.{:02}s → {:>3}.{:02}s]  {}",
                s.start_cs / 100,
                s.start_cs % 100,
                s.end_cs / 100,
                s.end_cs % 100,
                s.text
            );
        }

        // The whole point of the new path: timestamps came back and the chunk was
        // not silently discarded.
        assert!(
            !segs.is_empty(),
            "no segments — the skip guard likely discarded the chunk"
        );
        let mut prev_start = -1_i64;
        for s in &segs {
            assert!(s.start_cs >= 0, "negative start: {s:?}");
            assert!(s.end_cs >= s.start_cs, "end before start: {s:?}");
            assert!(
                s.start_cs >= prev_start,
                "segments not ordered by start: {s:?}"
            );
            assert!(!s.text.is_empty(), "empty segment text leaked through");
            prev_start = s.start_cs;
        }
    }
}
