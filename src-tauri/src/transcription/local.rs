//! Local (offline) speech-to-text backend for dragged audio files.
//!
//! Decodes an arbitrary audio file with symphonia → mono → 16 kHz, splits it
//! into whisper-sized chunks (cutting at quiet points to avoid mid-word splits),
//! and runs each chunk through the loaded local engine, joining the results.

use crate::models::{manager, storage};
use crate::whisper_local::audio;
use crate::whisper_local::inference::WhisperEngine;
use crate::whisper_local::LocalEngineHandle;
use crate::AppState;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

const SR: usize = 16_000;
/// Hard cap of decoded content per chunk (whisper processes one 30 s window when
/// `single_segment` is set; leave headroom for the 0.5 s pad below).
const MAX_CONTENT: usize = 29 * SR;
/// Preferred chunk length; the actual cut floats within ±SEARCH of this.
const TARGET: usize = 27 * SR;
const SEARCH: usize = 2 * SR;
const FRAME: usize = 1_600; // 100 ms energy window
const PAD: usize = SR / 2; // 0.5 s trailing silence (mirrors transcribe_local)

pub async fn transcribe(
    app: &AppHandle,
    state: State<'_, AppState>,
    input: &Path,
) -> Result<String, String> {
    let engine = ensure_engine(app, &state).await?;

    // Decode + resample are CPU/IO heavy — keep them off the async runtime.
    let input_owned = input.to_path_buf();
    let (raw, sample_rate, channels) = tokio::task::spawn_blocking(move || decode_audio(&input_owned))
        .await
        .map_err(|e| format!("שגיאת thread בפענוח אודיו: {e}"))??;

    let mono = audio::downmix_to_mono(&raw, channels);
    let pcm = tokio::task::spawn_blocking(move || audio::resample_to_16k(&mono, sample_rate))
        .await
        .map_err(|e| format!("שגיאת thread בהמרת קצב דגימה: {e}"))??;

    let chunks = split_chunks(&pcm);
    let total = chunks.len();
    let mut out = String::new();

    for (i, chunk) in chunks.into_iter().enumerate() {
        let _ = app.emit_to(
            "mic",
            "speakly://transcribe-progress",
            serde_json::json!({ "chunk": i + 1, "total": total }),
        );

        // Pad with silence so whisper.cpp emits a paired segment timestamp and
        // does not discard the chunk ("single timestamp ending - skip").
        let mut padded = chunk;
        padded.extend(std::iter::repeat(0.0_f32).take(PAD));

        let text = engine
            .transcribe(padded, "he")
            .await
            .map_err(|e| e.to_string())?;
        let t = text.trim();
        if !t.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(t);
        }
    }

    Ok(out)
}

/// Returns the loaded local engine, loading the previously-active (or first
/// installed) model on demand if the user runs file transcription while the live
/// engine is online.
async fn ensure_engine(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> Result<Arc<LocalEngineHandle>, String> {
    if let Some(e) = state.local_engine.lock().as_ref() {
        return Ok(Arc::clone(e));
    }

    let stg = crate::settings::load_or_init(app)?;
    let id = match stg.local_model_id {
        Some(id) => id,
        None => manager::list_installed(app)
            .into_iter()
            .next()
            .map(|m| m.id)
            .ok_or_else(|| {
                "לא נמצא מודל מקומי מותקן. הורד מודל בהגדרות → מנוע תמלול.".to_string()
            })?,
    };

    let meta_path = storage::model_dir(app, &id).join("meta.json");
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

/// Decodes any symphonia-supported audio file to interleaved f32 samples.
/// Returns `(samples, sample_rate, channel_count)`.
fn decode_audio(input: &Path) -> Result<(Vec<f32>, u32, usize), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(input).map_err(|e| format!("שגיאה בפתיחת הקובץ: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = input.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("פורמט אודיו לא נתמך או קובץ פגום: {e}"))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "לא נמצא ערוץ אודיו בקובץ".to_string())?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(16_000);
    let mut channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("קודק אודיו לא נתמך: {e}"))?;

    let mut samples: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // EOF / end-of-stream → done.
            Err(_) => break,
        };
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
            // Recoverable decode glitch → skip this packet.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }

    if samples.is_empty() {
        return Err("לא נמצא אודיו לפענוח בקובץ".into());
    }
    Ok((samples, sample_rate, channels))
}

/// Splits 16 kHz mono samples into ≤MAX_CONTENT windows, cutting at the quietest
/// 100 ms frame near each TARGET boundary so words aren't sliced mid-syllable.
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
