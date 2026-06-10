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

/// One spoken word with chunk-relative time bounds (centiseconds), assembled from
/// whisper.cpp token timestamps by [`WhisperEngine::transcribe_segments_words`].
/// The video pipeline shifts these to absolute time and writes them to the
/// `words.json` sidecar (karaoke burn-in style).
#[derive(Debug, Clone, PartialEq)]
pub struct WordSpan {
    pub text: String,
    pub t0_cs: i64,
    pub t1_cs: i64,
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

        // Trailing silence pad (see doc comment) — satisfies the skip guard now that
        // timestamps are on.
        let mut padded = Vec::with_capacity(audio_16k_mono.len() + PAD_SAMPLES);
        padded.extend_from_slice(audio_16k_mono);
        padded.extend(std::iter::repeat(0.0_f32).take(PAD_SAMPLES));

        state
            .full(Self::segment_params(lang), &padded)
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

    /// Like [`Self::transcribe_segments`], but additionally extracts **word-level**
    /// timestamps from whisper.cpp's per-token timings (`token_timestamps=true`,
    /// a post-pass heuristic — segmentation and text are unchanged). Backs the
    /// karaoke burn-in style's `words.json` sidecar; the SRT itself is still built
    /// from the returned segments exactly as before.
    ///
    /// Tokens are BPE byte pieces: a token whose bytes start with `b' '` opens a
    /// new word; bytes are accumulated per word (Hebrew codepoints can split
    /// *across* tokens, so per-token lossy decoding would corrupt them) and decoded
    /// once per word. Special tokens (`[_BEG_]`, `<|...|>`) are skipped.
    pub fn transcribe_segments_words(
        &self,
        audio_16k_mono: &[f32],
        lang: &str,
    ) -> Result<(Vec<Segment>, Vec<WordSpan>), EngineError> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| EngineError::Transcribe(e.to_string()))?;

        let mut params = Self::segment_params(lang);
        params.set_token_timestamps(true);

        let mut padded = Vec::with_capacity(audio_16k_mono.len() + PAD_SAMPLES);
        padded.extend_from_slice(audio_16k_mono);
        padded.extend(std::iter::repeat(0.0_f32).take(PAD_SAMPLES));

        state
            .full(params, &padded)
            .map_err(|e| EngineError::Transcribe(e.to_string()))?;

        let mut segments = Vec::new();
        let mut words = Vec::new();
        for seg in state.as_iter() {
            let text = seg
                .to_str_lossy()
                .map_err(|e| EngineError::Transcribe(e.to_string()))?
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }

            // Group this segment's tokens into words.
            let mut cur_bytes: Vec<u8> = Vec::new();
            let mut cur_t0: i64 = 0;
            let mut cur_t1: i64 = 0;
            let flush = |bytes: &mut Vec<u8>, t0: i64, t1: i64, out: &mut Vec<WordSpan>| {
                let w = String::from_utf8_lossy(bytes).trim().to_string();
                bytes.clear();
                if !w.is_empty() {
                    out.push(WordSpan {
                        text: w,
                        t0_cs: t0.max(0),
                        t1_cs: t1.max(t0.max(0)),
                    });
                }
            };
            for i in 0..seg.n_tokens() {
                let Some(tok) = seg.get_token(i) else { continue };
                let Ok(bytes) = tok.to_bytes() else { continue };
                // Special markers render as "[_...]" or "<|...|>" — never words.
                if bytes.starts_with(b"[_") || bytes.starts_with(b"<|") {
                    continue;
                }
                let data = tok.token_data();
                if bytes.starts_with(b" ") && !cur_bytes.is_empty() {
                    flush(&mut cur_bytes, cur_t0, cur_t1, &mut words);
                }
                if cur_bytes.is_empty() {
                    cur_t0 = data.t0;
                }
                cur_t1 = data.t1;
                cur_bytes.extend_from_slice(bytes);
            }
            flush(&mut cur_bytes, cur_t0, cur_t1, &mut words);

            segments.push(Segment {
                start_cs: seg.start_timestamp(),
                end_cs: seg.end_timestamp(),
                text,
            });
        }
        Ok((segments, words))
    }

    /// The shared `FullParams` for the timestamped (video→SRT) paths. Differs from
    /// `transcribe` in exactly two flags: multi-segment + timestamps enabled.
    fn segment_params(lang: &str) -> FullParams<'_, '_> {
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
        params
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

    /// Same harness as [`transcribe_segments_smoke`], for the word-timestamps
    /// variant backing the karaoke burn style. Validates the token→word grouping
    /// against a real model + clip: words come back, are time-ordered, and their
    /// concatenation matches the segment texts (modulo whitespace). Run:
    ///   $env:TIMLULI_TEST_MODEL = …ggml-model.bin ; $env:TIMLULI_TEST_AUDIO = …clip
    ///   cargo test --manifest-path src-tauri/Cargo.toml transcribe_segments_words_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn transcribe_segments_words_smoke() {
        let model = std::env::var("TIMLULI_TEST_MODEL")
            .expect("set TIMLULI_TEST_MODEL to an installed GGML model file path");
        let audio_path = std::env::var("TIMLULI_TEST_AUDIO")
            .expect("set TIMLULI_TEST_AUDIO to a speech clip (mp3/mp4/wav/m4a…)");

        let mut audio = decode_to_16k_mono(&PathBuf::from(&audio_path));
        assert!(!audio.is_empty(), "decoded to zero samples");
        let cap = 28 * 16_000;
        if audio.len() > cap {
            audio.truncate(cap);
        }

        let engine =
            WhisperEngine::load(&PathBuf::from(&model), "test".into()).expect("load model");
        let (segs, words) = engine
            .transcribe_segments_words(&audio, "he")
            .expect("transcribe_segments_words returned an error");

        println!(
            "=== {} segment(s), {} word(s) ===",
            segs.len(),
            words.len()
        );
        for w in &words {
            println!(
                "  [{:>3}.{:02}s → {:>3}.{:02}s]  {}",
                w.t0_cs / 100,
                w.t0_cs % 100,
                w.t1_cs / 100,
                w.t1_cs % 100,
                w.text
            );
        }

        assert!(!segs.is_empty(), "no segments");
        assert!(!words.is_empty(), "no words extracted from tokens");
        let mut prev_t0 = -1_i64;
        for w in &words {
            assert!(w.t1_cs >= w.t0_cs, "word end before start: {w:?}");
            assert!(w.t0_cs >= prev_t0, "words not time-ordered: {w:?}");
            assert!(!w.text.trim().is_empty(), "blank word leaked");
            assert!(
                !w.text.contains('\u{FFFD}'),
                "UTF-8 corruption — token bytes split mid-codepoint were not healed: {w:?}"
            );
            prev_t0 = w.t0_cs;
        }
        // The words must spell the segments (the karaoke matcher relies on this).
        let seg_join: String = segs
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let word_join: String = words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(seg_join, word_join, "word stream diverges from segment text");
    }
}
