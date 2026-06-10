//! Video → SRT subtitle pipeline (isolated, additive). See the development spec at
//! `.claude/temp/SPEC_video_transcription.md`.
//!
//! Drag a video onto the mic (or pick it in the side panel) → extract audio with
//! ffmpeg → transcribe with the configured engine (local whisper / Groq cloud) →
//! assemble an `.srt` next to the source. A new, isolated module: it *calls* the
//! existing engines and infra but changes none of them, and is gated by
//! `settings.video_subtitles_enabled` (toggle off ⇒ today's behavior exactly).
//!
//! The shared segment currency is [`crate::whisper_local::inference::Segment`]
//! (centisecond bounds) — produced by the local engine and mapped from the Groq
//! response, then consumed by [`srt::build_srt`].

pub mod ffmpeg;
pub mod groq_srt;
pub mod srt;
pub mod words;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use crate::whisper_local::inference::{Segment, WhisperEngine};
use crate::whisper_local::LocalEngineHandle;
use crate::AppState;

const SR: usize = 16_000;
/// Hard cap of decoded content per chunk (whisper processes one 30 s window).
const MAX_CONTENT: usize = 29 * SR;
/// Preferred chunk length; the cut floats within ±SEARCH of this.
const TARGET: usize = 27 * SR;
const SEARCH: usize = 2 * SR;
const FRAME: usize = 1_600; // 100 ms energy window

/// Builds the `<stem>.srt` output path next to the source video — same name as the
/// source, just the `.srt` extension, so players (e.g. VLC) auto-load it for the
/// matching video file (`movie.mp4` → `movie.srt`).
fn output_srt_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("video");
    input.with_file_name(format!("{stem}.srt"))
}

/// Emits an additive progress tick. `phase` (`"extract"` | `"transcribe"`) is a new
/// optional field; existing listeners read only `chunk`/`total` and ignore it.
fn emit_progress(app: &AppHandle, chunk: usize, total: usize, phase: &str) {
    let payload = json!({ "chunk": chunk, "total": total, "phase": phase });
    let _ = app.emit_to("mic", "speakly://transcribe-progress", payload.clone());
    let _ = app.emit_to("panel", "speakly://transcribe-progress", payload);
}

/// Entry point: validate → ensure ffmpeg → extract → transcribe (engine dispatch) →
/// (optional) punctuate → assemble SRT → write next to source. Returns the SRT path.
pub async fn transcribe_video_to_srt(
    app: &AppHandle,
    state: State<'_, AppState>,
    path: &str,
) -> Result<String, String> {
    let input = PathBuf::from(path);
    if !input.exists() {
        return Err(format!("הקובץ לא נמצא: {path}"));
    }

    let ffmpeg = ffmpeg::resolve(app).ok_or_else(|| {
        "להפקת כתוביות מווידאו יש להתקין את ffmpeg. הורד אותו בהגדרות → מנוע תמלול.".to_string()
    })?;

    let backend = crate::settings::load_or_init(app)?.audio_file_engine;
    let tmp = std::env::temp_dir();
    let uid = uuid::Uuid::new_v4();

    emit_progress(app, 1, 1, "extract");

    // Word-level timestamps ride along for the karaoke burn-in style's
    // `words.json` sidecar — both engines produce them at zero extra cost.
    let mut timed_words: Vec<words::TimedWord> = Vec::new();
    let engine_name: &str;

    let mut segments: Vec<Segment> = match backend.as_str() {
        "whisper-local" => {
            engine_name = "whisper-local";
            let pcm_tmp = tmp.join(format!("timluli-vid-{uid}.pcm"));
            // ffmpeg is a blocking subprocess — keep it off the async runtime.
            let (ff, inp, out) = (ffmpeg.clone(), input.clone(), pcm_tmp.clone());
            tokio::task::spawn_blocking(move || ffmpeg::extract_pcm_f32le(&ff, &inp, &out))
                .await
                .map_err(|e| format!("שגיאת thread בחילוץ אודיו: {e}"))??;

            let pcm = ffmpeg::read_pcm_f32le(&pcm_tmp);
            let _ = std::fs::remove_file(&pcm_tmp);
            let pcm = pcm?;

            let engine = ensure_local_engine(app, &state).await?;
            let chunks = split_chunks(&pcm);
            let total = chunks.len().max(1);
            let mut segs: Vec<Segment> = Vec::new();
            let mut offset_samples: usize = 0;
            for (i, chunk) in chunks.into_iter().enumerate() {
                emit_progress(app, i + 1, total, "transcribe");
                let chunk_len = chunk.len();
                let (chunk_segs, chunk_words) = engine
                    .transcribe_segments_words(chunk, "he")
                    .await
                    .map_err(|e| e.to_string())?;
                // Shift chunk-relative timestamps to absolute: samples/16000*100 cs.
                let offset_cs = (offset_samples / 160) as i64;
                for mut s in chunk_segs {
                    s.start_cs += offset_cs;
                    s.end_cs += offset_cs;
                    segs.push(s);
                }
                for w in chunk_words {
                    timed_words.push(words::TimedWord {
                        w: w.text,
                        t0_cs: w.t0_cs + offset_cs,
                        t1_cs: w.t1_cs + offset_cs,
                    });
                }
                offset_samples += chunk_len;
            }
            segs
        }
        // Default to the cloud backend for any other value (mirrors the .txt path).
        _ => {
            engine_name = "groq";
            let flac_tmp = tmp.join(format!("timluli-vid-{uid}.flac"));
            let (ff, inp, out) = (ffmpeg.clone(), input.clone(), flac_tmp.clone());
            tokio::task::spawn_blocking(move || ffmpeg::extract_flac(&ff, &inp, &out))
                .await
                .map_err(|e| format!("שגיאת thread בחילוץ אודיו: {e}"))??;

            emit_progress(app, 1, 1, "transcribe");
            let result = groq_srt::transcribe_to_segments(app, &flac_tmp).await;
            let _ = std::fs::remove_file(&flac_tmp);
            let (segs, ws) = result?;
            timed_words = ws;
            segs
        }
    };

    if segments.is_empty() {
        return Err("לא זוהה דיבור בקובץ הווידאו".into());
    }

    // Soft punctuation hook: applied per cue only if the engine is already loaded
    // (opt-in, default off) — never a hard dependency. No sentence-newlines in SRT.
    for seg in &mut segments {
        let t = std::mem::take(&mut seg.text);
        seg.text = crate::commands_punct::punctuate_if_ready(state.inner(), t, false, false).await;
    }

    let srt = srt::build_srt(&segments);
    let out_path = output_srt_path(&input);
    std::fs::write(&out_path, srt).map_err(|e| format!("שגיאה בכתיבת קובץ הכתוביות: {e}"))?;

    // Best-effort `words.json` sidecar (karaoke burn-in style). The words carry
    // the raw (pre-punctuation) text — the burn side normalizes punctuation away
    // when matching. A failure here must never fail the SRT itself.
    if !timed_words.is_empty() {
        let sidecar = words::sidecar_path(&out_path);
        if let Err(e) = words::write(&sidecar, engine_name, &timed_words) {
            log::warn!("words.json sidecar write failed (SRT unaffected): {e}");
        }
    }

    Ok(out_path.to_string_lossy().into_owned())
}

/// Returns the loaded local engine, lazy-loading the previously-active (or first
/// installed) model on demand. Mirrors `transcription::local::ensure_engine`,
/// duplicated here to keep the audio-file path untouched.
async fn ensure_local_engine(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> Result<Arc<LocalEngineHandle>, String> {
    if let Some(e) = state.local_engine.lock().as_ref() {
        return Ok(Arc::clone(e));
    }

    let stg = crate::settings::load_or_init(app)?;
    let id = match stg.local_model_id {
        Some(id) => id,
        None => crate::models::manager::list_installed(app)
            .into_iter()
            .next()
            .map(|m| m.id)
            .ok_or_else(|| "לא נמצא מודל מקומי מותקן. הורד מודל בהגדרות → מנוע תמלול.".to_string())?,
    };

    let meta_path = crate::models::storage::model_dir(app, &id).join("meta.json");
    let meta_str = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("שגיאה בקריאת מטא-נתוני מודל: {e}"))?;
    let meta: crate::models::types::InstalledModel = serde_json::from_str(&meta_str)
        .map_err(|e| format!("שגיאה בפענוח מטא-נתוני מודל: {e}"))?;
    let model_path = PathBuf::from(&meta.file_path);
    let model_id = id.clone();

    let engine = tokio::task::spawn_blocking(move || WhisperEngine::load(&model_path, model_id))
        .await
        .map_err(|e| format!("שגיאת thread בטעינת מנוע: {e}"))?
        .map_err(|e| e.to_string())?;

    let handle = Arc::new(LocalEngineHandle::new(engine));
    *state.local_engine.lock() = Some(Arc::clone(&handle));
    Ok(handle)
}

/// Splits 16 kHz mono samples into ≤MAX_CONTENT windows, cutting at the quietest
/// 100 ms frame near each TARGET boundary. Duplicated from `transcription::local`
/// to avoid touching the audio-file path.
fn split_chunks(samples: &[f32]) -> Vec<Vec<f32>> {
    let n = samples.len();
    let mut chunks = Vec::new();
    let mut start = 0;
    while n - start > MAX_CONTENT {
        let lo = start + TARGET - SEARCH;
        let hi = (start + TARGET + SEARCH).min(start + MAX_CONTENT).min(n);
        let cut = quietest_boundary(samples, lo, hi);
        chunks.push(samples[start..cut].to_vec());
        start = cut;
    }
    if start < n {
        chunks.push(samples[start..].to_vec());
    }
    chunks
}

/// Finds the center of the lowest-energy FRAME within `[lo, hi)`.
fn quietest_boundary(samples: &[f32], lo: usize, hi: usize) -> usize {
    let mut best = lo;
    let mut best_energy = f32::MAX;
    let mut i = lo;
    while i + FRAME <= hi {
        let energy: f32 = samples[i..i + FRAME].iter().map(|s| s * s).sum();
        if energy < best_energy {
            best_energy = energy;
            best = i;
        }
        i += FRAME;
    }
    (best + FRAME / 2).min(hi)
}

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use crate::whisper_local::inference::WhisperEngine;
    use std::path::Path;

    /// Decodes the first ~140 s of a video's audio to 16 kHz mono f32 via symphonia
    /// (mp4/aac supported), reusing the crate's downmix/resample. Stands in for the
    /// ffmpeg extraction step — it yields the same f32 the local engine consumes — so
    /// the test runs without an ffmpeg binary present.
    fn decode_head_16k_mono(path: &Path) -> Vec<f32> {
        use symphonia::core::audio::SampleBuffer;
        use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
        use symphonia::core::errors::Error as SymphoniaError;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let file = std::fs::File::open(path).expect("open video");
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .expect("probe video format");
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
            // Early stop — no need to decode a full lecture for a 120 s test window.
            if samples.len() > 140 * sample_rate as usize * channels {
                break;
            }
        }
        let mono = crate::whisper_local::audio::downmix_to_mono(&samples, channels);
        crate::whisper_local::audio::resample_to_16k(&mono, sample_rate).expect("resample")
    }

    /// End-to-end local pipeline on a real video, bypassing only the Tauri + ffmpeg
    /// layers: decode → chunking → `transcribe_segments` (offset-stitched) →
    /// `srt::build_srt` → write `<stem>.he.srt` next to the source. Capped to the
    /// first 120 s so a long lecture finishes quickly. `#[ignore]`d — set:
    ///   TIMLULI_TEST_MODEL = installed GGML model path
    ///   TIMLULI_TEST_VIDEO = path to a video file
    ///   cargo test --manifest-path src-tauri/Cargo.toml local_video_to_srt_e2e -- --ignored --nocapture
    #[test]
    #[ignore]
    fn local_video_to_srt_e2e() {
        let model = std::env::var("TIMLULI_TEST_MODEL").expect("TIMLULI_TEST_MODEL");
        let video = PathBuf::from(std::env::var("TIMLULI_TEST_VIDEO").expect("TIMLULI_TEST_VIDEO"));

        let mut pcm = decode_head_16k_mono(&video);
        let cap = 120 * SR;
        if pcm.len() > cap {
            pcm.truncate(cap);
        }
        println!("\n=== feeding {:.0}s of audio ===", pcm.len() as f32 / SR as f32);

        let engine = WhisperEngine::load(&PathBuf::from(&model), "test".into()).expect("load model");
        let chunks = split_chunks(&pcm);
        let total = chunks.len();
        let mut segments: Vec<Segment> = Vec::new();
        let mut offset_samples = 0usize;
        for (i, chunk) in chunks.into_iter().enumerate() {
            println!("transcribing chunk {}/{}…", i + 1, total);
            let len = chunk.len();
            let cs = engine.transcribe_segments(&chunk, "he").expect("transcribe_segments");
            let offset_cs = (offset_samples / 160) as i64;
            for mut s in cs {
                s.start_cs += offset_cs;
                s.end_cs += offset_cs;
                segments.push(s);
            }
            offset_samples += len;
        }

        assert!(!segments.is_empty(), "no segments produced");
        let srt = srt::build_srt(&segments);
        let out = output_srt_path(&video);
        std::fs::write(&out, &srt).expect("write srt");
        println!(
            "\n=== wrote {} ({} cues) ===\n",
            out.display(),
            srt.matches(" --> ").count()
        );
        println!("{}", &srt[..srt.len().min(1800)]);
    }
}
