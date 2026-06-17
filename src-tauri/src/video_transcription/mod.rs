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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use crate::whisper_local::inference::{Segment, WhisperEngine};
use crate::whisper_local::LocalEngineHandle;
use crate::AppState;

const SR: usize = 16_000;
/// Cloud STT slice length (seconds). With `-sample_fmt s16` the FLAC is bounded to
/// 16000×2 = 32 kB/s, so a 5 min slice is ≤ ~9.6 MB — comfortably under Groq's
/// 25 MB upload cap [[reference: user requirement]] with margin to spare. Small
/// slices also let many transcribe in parallel (see `CLOUD_STT_CONCURRENCY`),
/// cutting wall-clock on a full-length film. Videos at/under this length keep the
/// original single-request path unchanged.
const CLOUD_CHUNK_SECS: f64 = 300.0;
/// Hard ceiling on any uploaded slice (Groq rejects >25 MB). At 32 kB/s a slice can
/// only reach this past ~13 min, which `CLOUD_CHUNK_SECS` never approaches — so this
/// is a defensive guard, logged if ever hit.
const CLOUD_MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;
/// Bounded parallelism for cloud slice transcription, sized per tier from a measured
/// concurrency sweep against whisper-large-v3:
///   - PAID: throughput plateaus at ~8 concurrent (Groq caps one key at ~2 req/s ≈
///     120× realtime; 8→32 adds <10%), with 0 rate-limits even at 32. 12 sits just
///     past the knee for headroom — a 2 h film transcribes in ~60 s.
///   - FREE: the 7200 audio-s/hour budget is the hard ceiling; C=4 is the fastest
///     burst that stays under it (C=8 already drew 429s in the sweep). The rate-aware
///     retry in `groq_srt` honors `Retry-After`, so even when the budget is hit no
///     slice is dropped — it just paces to the refill rate.
const CLOUD_STT_CONCURRENCY_PAID: usize = 12;
const CLOUD_STT_CONCURRENCY_FREE: usize = 4;
/// How far (seconds) a cloud slice boundary may move from its ideal position to
/// land inside a detected silence, so no spoken word is split across a seam. A
/// slice can thus reach `CLOUD_CHUNK_SECS + CLOUD_CUT_TOLERANCE`; at ~8 kB/s that
/// is ~2.6 MB, still far under the 25 MB cap. No silence near a boundary ⇒ the cut
/// stays at the ideal position (today's fixed slicing).
const CLOUD_CUT_TOLERANCE: f64 = 30.0;
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
    lang: &str,
) -> Result<String, String> {
    let input = PathBuf::from(path);
    if !input.exists() {
        return Err(format!("הקובץ לא נמצא: {path}"));
    }

    let ffmpeg = ffmpeg::resolve(app).ok_or_else(|| {
        "להפקת כתוביות מווידאו יש להתקין את ffmpeg. הורד אותו בהגדרות → מנוע תמלול.".to_string()
    })?;

    // Pick the channel→mono downmix from the source layout: multichannel sources
    // (5.1/7.1/…) isolate the front-center dialogue (cleaner, ~10 dB louder speech →
    // better WER + gender); stereo/mono keep the standard mono fold. Probed once and
    // reused by every extraction below (local PCM, cloud MP3 slices, gender re-extract).
    let downmix = ffmpeg::probe_downmix(&ffmpeg, &input);
    log::info!("audio downmix: {downmix:?}");

    let stg = crate::settings::load_or_init(app)?;
    let backend = stg.audio_file_engine;
    let tmp = std::env::temp_dir();
    let uid = uuid::Uuid::new_v4();

    emit_progress(app, 1, 1, "extract");

    // Word-level timestamps ride along for the karaoke burn-in style's
    // `words.json` sidecar — both engines produce them at zero extra cost.
    let mut timed_words: Vec<words::TimedWord> = Vec::new();
    let engine_name: &str;
    // PCM kept for the gender-classification pass (local path already decodes
    // it; the cloud path extracts FLAC only, so it re-extracts on demand below).
    let mut gender_pcm: Option<Vec<f32>> = None;

    let mut segments: Vec<Segment> = match backend.as_str() {
        "whisper-local" => {
            engine_name = "whisper-local";
            let pcm_tmp = tmp.join(format!("timluli-vid-{uid}.pcm"));
            // ffmpeg is a blocking subprocess — keep it off the async runtime.
            let (ff, inp, out) = (ffmpeg.clone(), input.clone(), pcm_tmp.clone());
            tokio::task::spawn_blocking(move || ffmpeg::extract_pcm_f32le(&ff, &inp, &out, downmix))
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
                    .transcribe_segments_words(chunk, lang.to_string())
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
            gender_pcm = Some(pcm);
            segs
        }
        // Default to the cloud backend (mirrors the .txt path). Audio is encoded to
        // compact MP3; anything longer than one slice is transcribed in PARALLEL
        // slices (bounded concurrency), each slice's timestamps offset back to
        // absolute time — so a full-length film finishes in a fraction of the
        // sequential time and every upload stays well under Groq's 25 MB cap. Short
        // videos take the single-request path.
        _ => {
            engine_name = "groq";
            let duration = ffmpeg::probe_duration(&ffmpeg, &input).unwrap_or_else(|e| {
                log::warn!("ffmpeg duration probe failed ({e}); using single request");
                0.0
            });

            if duration > CLOUD_CHUNK_SECS {
                // Slice the timeline into ~CLOUD_CHUNK_SECS windows, but land every cut
                // inside a detected silence when one is near — so no word is split across
                // a seam (the v1 hard-boundary caveat). One cheap ffmpeg `silencedetect`
                // analysis pass; an empty result transparently falls back to fixed cuts.
                let silences = ffmpeg::detect_silences(&ffmpeg, &input);
                log::info!("cloud chunking: {} silence interval(s) detected", silences.len());
                let cuts = plan_slice_cuts(duration, CLOUD_CHUNK_SECS, CLOUD_CUT_TOLERANCE, &silences);
                let mut bounds: Vec<f64> = Vec::with_capacity(cuts.len() + 2);
                bounds.push(0.0);
                bounds.extend(cuts);
                bounds.push(duration);
                // (index, start, dur) per non-empty window between consecutive bounds.
                let specs: Vec<(usize, f64, f64)> = bounds
                    .windows(2)
                    .filter(|w| w[1] > w[0])
                    .enumerate()
                    .map(|(i, w)| (i, w[0], w[1] - w[0]))
                    .collect();
                let total = specs.len();
                let completed = AtomicUsize::new(0);
                // Concurrency by the key's tier (groq_paid). Each slice self-paces on
                // 429 via the rate-aware groq_srt retry, so this is the burst size, not
                // a hard limiter.
                let concurrency = if stg.groq_paid {
                    CLOUD_STT_CONCURRENCY_PAID
                } else {
                    CLOUD_STT_CONCURRENCY_FREE
                };

                // Each future extracts its own MP3, transcribes (with retry), deletes
                // the temp, and returns its index + time-shifted segments/words. Bounded
                // concurrency overlaps upload + inference across slices.
                let outcomes: Vec<Result<(usize, Vec<Segment>, Vec<words::TimedWord>), String>> =
                    futures_util::stream::iter(specs.into_iter().map(|(i, start, dur)| {
                        let ffmpeg = &ffmpeg;
                        let input = &input;
                        let tmp = &tmp;
                        let completed = &completed;
                        let dm = downmix;
                        async move {
                            let mp3 = tmp.join(format!("timluli-vid-{uid}-{i}.mp3"));
                            let (ff, inp, out) = (ffmpeg.clone(), input.clone(), mp3.clone());
                            tokio::task::spawn_blocking(move || {
                                ffmpeg::extract_mp3_range(&ff, &inp, start, dur, &out, dm)
                            })
                            .await
                            .map_err(|e| format!("שגיאת thread בחילוץ אודיו: {e}"))??;

                            if let Ok(meta) = std::fs::metadata(&mp3) {
                                if meta.len() > CLOUD_MAX_UPLOAD_BYTES {
                                    log::warn!("cloud slice {i}: {} bytes exceeds 25 MB", meta.len());
                                }
                            }

                            let result = groq_srt::transcribe_to_segments(app, &mp3, lang).await;
                            let _ = std::fs::remove_file(&mp3);
                            let (mut cs, mut ws) = result?;

                            let offset_cs = (start * 100.0).round() as i64;
                            for s in &mut cs {
                                s.start_cs += offset_cs;
                                s.end_cs += offset_cs;
                            }
                            for w in &mut ws {
                                w.t0_cs += offset_cs;
                                w.t1_cs += offset_cs;
                            }
                            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                            emit_progress(app, done, total, "transcribe");
                            Ok((i, cs, ws))
                        }
                    }))
                    .buffer_unordered(concurrency)
                    .collect()
                    .await;

                // Reassemble in timeline order. Tolerate a failed slice (a gap) but
                // not a total wipeout.
                let mut ok: Vec<(usize, Vec<Segment>, Vec<words::TimedWord>)> = Vec::new();
                let mut failed = 0usize;
                let mut last_err = String::new();
                for outcome in outcomes {
                    match outcome {
                        Ok(v) => ok.push(v),
                        Err(e) => {
                            failed += 1;
                            last_err = e;
                        }
                    }
                }
                if ok.is_empty() {
                    return Err(format!("התמלול בענן נכשל: {last_err}"));
                }
                if failed > 0 {
                    log::warn!("{failed}/{total} cloud slice(s) failed; subtitles may have gaps");
                }
                ok.sort_by_key(|(i, _, _)| *i);
                let mut segs: Vec<Segment> = Vec::new();
                for (_, cs, ws) in ok {
                    segs.extend(cs);
                    timed_words.extend(ws);
                }
                segs
            } else {
                let mp3_tmp = tmp.join(format!("timluli-vid-{uid}.mp3"));
                let (ff, inp, out) = (ffmpeg.clone(), input.clone(), mp3_tmp.clone());
                tokio::task::spawn_blocking(move || ffmpeg::extract_mp3(&ff, &inp, &out, downmix))
                    .await
                    .map_err(|e| format!("שגיאת thread בחילוץ אודיו: {e}"))??;

                emit_progress(app, 1, 1, "transcribe");
                let result = groq_srt::transcribe_to_segments(app, &mp3_tmp, lang).await;
                let _ = std::fs::remove_file(&mp3_tmp);
                let (segs, ws) = result?;
                timed_words = ws;
                segs
            }
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

    let cues = srt::build_cues(&segments);
    let srt = srt::render_srt(&cues);
    let out_path = output_srt_path(&input);
    std::fs::write(&out_path, srt).map_err(|e| format!("שגיאה בכתיבת קובץ הכתוביות: {e}"))?;

    // A genders sidecar from a previous run describes the *old* cue timings —
    // remove it unconditionally so a regenerated SRT can't pair with stale tags.
    let genders_path = crate::gender_f0::sidecar_path(&out_path);
    let _ = std::fs::remove_file(&genders_path);

    // Speaker-gender classification (opt-in): per-cue F0 analysis over the extracted
    // PCM, optionally augmented by the in-process ONNX classifier when installed,
    // persisted as `<stem>.genders.json` for the subtitle translation path. Strictly
    // best-effort — any failure logs and moves on.
    if stg.gender_aware_translation {
        match gender_pcm_or_extract(gender_pcm, &ffmpeg, &input, &tmp, uid, downmix).await {
            Ok(pcm) => {
                let windows: Vec<(i64, i64)> = cues.iter().map(|(s, e, _)| (*s, *e)).collect();
                // Cue texts for the transcript-based (grammar) gender signal.
                let texts: Vec<String> = cues.iter().map(|(_, _, t)| t.clone()).collect();
                // Optional ONNX gender classifier (loaded only when enabled +
                // installed): clone the handle out before the blocking pass.
                let gender_engine = state.gender_engine.lock().as_ref().map(Arc::clone);
                // CPU work off the async runtime — seconds for F0 alone, a few
                // minutes when the ONNX classifier also runs over a full movie.
                let classified = tokio::task::spawn_blocking(move || {
                    let mut cg = crate::gender_f0::classify_cues(&pcm, &windows);
                    // When the model is loaded, its confident labels override the F0
                    // guess for the cases F0 misses (adult-male octave errors, the
                    // 155–175 Hz overlap). Quiet/short cues yield None → F0 stands.
                    if let Some(eng) = gender_engine {
                        for (c, o) in cg.iter_mut().zip(eng.classify_windows(&pcm, &windows)) {
                            if let Some((g, conf)) = o {
                                if conf >= crate::gender_onnx::ACCEPT_CONFIDENCE {
                                    c.gender = g;
                                }
                            }
                        }
                    }
                    // Highest-priority signal: the transcript's own first-person gender
                    // morphology (Hebrew). A near-certain linguistic fact about the
                    // speaker — and the ONLY cue that can correct children, whom audio
                    // fundamentally cannot sex. Overrides the acoustic guess.
                    for (c, txt) in cg.iter_mut().zip(&texts) {
                        if let Some(g) = crate::gender_text::infer_speaker_gender(txt) {
                            c.gender = g;
                        }
                    }
                    crate::gender_f0::smooth(&mut cg);
                    cg
                })
                .await;
                match classified {
                    Ok(cg) => match crate::gender_f0::write_sidecar(&genders_path, &cg) {
                        Ok(true) => log::info!("gender sidecar written: {}", genders_path.display()),
                        Ok(false) => log::info!("gender analysis: no confident cues, no sidecar"),
                        Err(e) => log::warn!("gender sidecar write failed (SRT unaffected): {e}"),
                    },
                    Err(e) => log::warn!("gender analysis thread failed (SRT unaffected): {e}"),
                }
            }
            Err(e) => log::warn!("gender analysis skipped — PCM unavailable: {e}"),
        }
    }

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

/// PCM for the gender pass: the local path's already-decoded samples when
/// available, otherwise a fresh ffmpeg extraction (the cloud path only produced
/// a FLAC, which was uploaded and deleted). The extra extraction is the cheap
/// part of the pipeline and only runs when the feature is enabled.
async fn gender_pcm_or_extract(
    existing: Option<Vec<f32>>,
    ffmpeg: &Path,
    input: &Path,
    tmp: &Path,
    uid: uuid::Uuid,
    dm: ffmpeg::Downmix,
) -> Result<Vec<f32>, String> {
    if let Some(pcm) = existing {
        return Ok(pcm);
    }
    let pcm_tmp = tmp.join(format!("timluli-gen-{uid}.pcm"));
    let (ff, inp, out) = (ffmpeg.to_path_buf(), input.to_path_buf(), pcm_tmp.clone());
    tokio::task::spawn_blocking(move || ffmpeg::extract_pcm_f32le(&ff, &inp, &out, dm))
        .await
        .map_err(|e| format!("שגיאת thread בחילוץ אודיו: {e}"))??;
    let pcm = ffmpeg::read_pcm_f32le(&pcm_tmp);
    let _ = std::fs::remove_file(&pcm_tmp);
    pcm
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

/// Splits 16 kHz mono samples into ≤MAX_CONTENT windows, cutting inside a real
/// speech pause near each TARGET boundary when one exists (else the quietest
/// frame), so a chunk seam never falls mid-word. Duplicated from
/// `transcription::local` to avoid touching the audio-file path.
fn split_chunks(samples: &[f32]) -> Vec<Vec<f32>> {
    let n = samples.len();
    let mut chunks = Vec::new();
    let mut start = 0;
    while n - start > MAX_CONTENT {
        // Widen the search below TARGET so an early pause is preferred before
        // falling back to the quietest frame.
        let lo = start + TARGET - 2 * SEARCH;
        let hi = (start + TARGET + SEARCH).min(start + MAX_CONTENT).min(n);
        let cut = silence_boundary(samples, lo, hi);
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

/// Picks a chunk boundary in `[lo, hi)` at the centre of the longest real speech
/// pause — a run of consecutive 100 ms frames whose RMS is below a silence floor
/// (~-34 dBFS, mirroring the cloud path's ffmpeg `silencedetect`). When the window
/// holds no such pause it falls back to [`quietest_boundary`] (the previous
/// behaviour), so a cut is never worse than before, only better when a true pause
/// exists.
fn silence_boundary(samples: &[f32], lo: usize, hi: usize) -> usize {
    const SILENCE_RMS: f32 = 0.02; // ≈ -34 dBFS over a 100 ms frame
    let (mut best_start, mut best_len) = (0usize, 0usize);
    let (mut run_start, mut run_len) = (0usize, 0usize);
    let mut i = lo;
    while i + FRAME <= hi {
        let rms = (samples[i..i + FRAME].iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        if rms < SILENCE_RMS {
            if run_len == 0 {
                run_start = i;
            }
            run_len += 1;
            if run_len > best_len {
                best_len = run_len;
                best_start = run_start;
            }
        } else {
            run_len = 0;
        }
        i += FRAME;
    }
    if best_len > 0 {
        (best_start + best_len * FRAME / 2).min(hi)
    } else {
        quietest_boundary(samples, lo, hi)
    }
}

/// Plans cloud-slice cut points (absolute seconds, ascending) so each slice is
/// about `target` seconds, but every cut lands inside a detected silence when one
/// lies within `tol` of the ideal boundary — so no word straddles a seam. With no
/// silence near a boundary the cut stays at the ideal position (identical to fixed
/// slicing). `silences` are `(start, end)` seconds from [`ffmpeg::detect_silences`].
fn plan_slice_cuts(duration: f64, target: f64, tol: f64, silences: &[(f64, f64)]) -> Vec<f64> {
    let mut cuts = Vec::new();
    let mut prev = 0.0_f64;
    while prev + target < duration - tol {
        let ideal = prev + target;
        let cut = silences
            .iter()
            .map(|&(s, e)| ideal.clamp(s, e)) // nearest point inside this silence
            .filter(|&c| c > prev + 1.0 && c < duration && (c - ideal).abs() <= tol)
            .min_by(|a, b| {
                (a - ideal)
                    .abs()
                    .partial_cmp(&(b - ideal).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(ideal);
        if cut <= prev + 1.0 {
            break;
        }
        cuts.push(cut);
        prev = cut;
    }
    cuts
}

#[cfg(test)]
mod chunk_tests {
    use super::{plan_slice_cuts, silence_boundary, CLOUD_CUT_TOLERANCE, SR};

    #[test]
    fn cut_snaps_to_nearby_silence() {
        let cuts = plan_slice_cuts(600.0, 300.0, CLOUD_CUT_TOLERANCE, &[(305.0, 307.0)]);
        assert_eq!(cuts.len(), 1);
        assert!((cuts[0] - 305.0).abs() < 1e-6, "cut should snap into the silence: {cuts:?}");
    }

    #[test]
    fn no_silence_uses_ideal_boundary() {
        assert_eq!(plan_slice_cuts(600.0, 300.0, 30.0, &[]), vec![300.0]);
    }

    #[test]
    fn silence_outside_tolerance_is_ignored() {
        assert_eq!(plan_slice_cuts(600.0, 300.0, 30.0, &[(400.0, 402.0)]), vec![300.0]);
    }

    #[test]
    fn multiple_cuts_stay_ordered_and_within_duration() {
        let sil = vec![(298.0, 299.0), (596.0, 599.0), (900.0, 902.0)];
        let cuts = plan_slice_cuts(1000.0, 300.0, 30.0, &sil);
        assert!(cuts.windows(2).all(|w| w[1] > w[0]), "ascending: {cuts:?}");
        assert!(*cuts.last().unwrap() < 1000.0);
        assert_eq!(cuts, vec![299.0, 599.0, 900.0]);
    }

    /// Fills `[lo_t, hi_t)` seconds of `buf` with a 150 Hz tone (speech stand-in).
    fn tone(buf: &mut [f32], lo_t: f32, hi_t: f32) {
        for i in 0..buf.len() {
            let t = i as f32 / SR as f32;
            if t >= lo_t && t < hi_t {
                buf[i] = 0.3 * (2.0 * std::f32::consts::PI * 150.0 * t).sin();
            }
        }
    }

    #[test]
    fn boundary_lands_in_silent_gap() {
        // Speech everywhere except a 1 s silent gap [5 s, 6 s).
        let mut s = vec![0.0f32; 10 * SR];
        tone(&mut s, 0.0, 5.0);
        tone(&mut s, 6.0, 10.0);
        let cut_t = silence_boundary(&s, 4 * SR, 7 * SR) as f32 / SR as f32;
        assert!((5.0..6.0).contains(&cut_t), "cut at {cut_t}s not in the silent gap");
    }

    #[test]
    fn boundary_without_silence_stays_in_window() {
        let mut s = vec![0.0f32; 10 * SR];
        tone(&mut s, 0.0, 10.0);
        let (lo, hi) = (4 * SR, 7 * SR);
        let cut = silence_boundary(&s, lo, hi);
        assert!(cut >= lo && cut <= hi, "fallback cut must stay in window");
    }
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

    /// Stage-1 verification on a real film: extract a chunk from the MIDDLE, then
    /// exercise the actual silence-aware chunking code — `ffmpeg::detect_silences` +
    /// `plan_slice_cuts` (cloud path) and `silence_boundary` vs `quietest_boundary`
    /// (local path) — showing cuts land in real silences (no word-clipping). Reads
    /// the bundled ffmpeg from `%APPDATA%`. `#[ignore]`d — set the film path:
    ///   TIMLULI_TEST_VIDEO = path to a video file
    ///   cargo test --manifest-path src-tauri/Cargo.toml stage1_silence_chunking_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn stage1_silence_chunking_probe() {
        let ff = PathBuf::from(std::env::var("APPDATA").expect("APPDATA"))
            .join(r"studio.oliel.timluli\ffmpeg\ffmpeg.exe");
        let movie = PathBuf::from(
            std::env::var("TIMLULI_TEST_VIDEO").expect("set TIMLULI_TEST_VIDEO to a video path"),
        );
        assert!(ff.exists(), "ffmpeg not found at {}", ff.display());
        assert!(movie.exists(), "movie not found at {}", movie.display());

        let dur = ffmpeg::probe_duration(&ff, &movie).expect("probe duration");
        let mid = (dur / 2.0).floor();
        let chunk_secs = 600.0_f64;
        println!(
            "\n=== film {:.0}s ({:.0} min); chunk {:.0}s from the middle @ {:.0}s ===",
            dur,
            dur / 60.0,
            chunk_secs,
            mid
        );

        let tmp = std::env::temp_dir();
        let chunk_mp3 = tmp.join("stage1-mid.mp3");
        ffmpeg::extract_mp3_range(&ff, &movie, mid, chunk_secs, &chunk_mp3, ffmpeg::Downmix::Plain)
            .expect("extract mp3");

        // ── Cloud path: detect silences + plan slice cuts over the 600 s chunk ──────
        let silences = ffmpeg::detect_silences(&ff, &chunk_mp3);
        println!("\n[cloud] detected {} silence interval(s); first few:", silences.len());
        for s in silences.iter().take(6) {
            println!("    {:.2}-{:.2}s  ({:.2}s)", s.0, s.1, s.1 - s.0);
        }
        let cuts = plan_slice_cuts(chunk_secs, CLOUD_CHUNK_SECS, CLOUD_CUT_TOLERANCE, &silences);
        println!("[cloud] planned cuts: {:?}", cuts);
        let naive = CLOUD_CHUNK_SECS;
        let naive_in = silences.iter().any(|&(s, e)| naive >= s && naive <= e);
        println!("[cloud] naive fixed cut @ {:.0}s lands in a silence? {naive_in}", naive);
        for &c in &cuts {
            let inside = silences.iter().any(|&(s, e)| c >= s - 0.05 && c <= e + 0.05);
            println!("    cut @ {:.2}s inside a silence? {inside}  (Δ from naive {:.0}s = {:.2}s)", c, naive, c - naive);
        }
        assert!(!silences.is_empty(), "no silences in a 10-min film chunk — silencedetect misconfigured");
        assert!(
            cuts.iter().all(|&c| (c - CLOUD_CHUNK_SECS).abs() <= CLOUD_CUT_TOLERANCE + 0.001),
            "a cut drifted beyond tolerance: {cuts:?}"
        );

        // ── Local path: PCM of the chunk → silence_boundary vs the old quietest cut ─
        let chunk_pcm = tmp.join("stage1-mid.pcm");
        ffmpeg::extract_pcm_f32le(&ff, &chunk_mp3, &chunk_pcm, ffmpeg::Downmix::Plain)
            .expect("extract pcm");
        let pcm = ffmpeg::read_pcm_f32le(&chunk_pcm).expect("read pcm");
        println!("\n[local] chunk pcm: {} samples = {:.1}s", pcm.len(), pcm.len() as f32 / SR as f32);
        let rms_at = |center: usize| -> f32 {
            let lo = center.saturating_sub(FRAME / 2);
            let hi = (lo + FRAME).min(pcm.len());
            if hi <= lo {
                return 0.0;
            }
            (pcm[lo..hi].iter().map(|s| s * s).sum::<f32>() / (hi - lo) as f32).sqrt()
        };
        if pcm.len() > TARGET + SEARCH {
            let hi = (TARGET + SEARCH).min(MAX_CONTENT).min(pcm.len());
            let smart = silence_boundary(&pcm, TARGET - 2 * SEARCH, hi);
            let old = quietest_boundary(&pcm, TARGET - SEARCH, hi);
            println!(
                "[local] first boundary: smart @ {:.2}s (RMS {:.4})  vs  old-quietest @ {:.2}s (RMS {:.4})",
                smart as f32 / SR as f32,
                rms_at(smart),
                old as f32 / SR as f32,
                rms_at(old)
            );
        }
        let chunks = split_chunks(&pcm);
        println!("[local] split into {} chunks; boundary RMS values (low = quiet cut):", chunks.len());
        let mut acc = 0usize;
        for (i, ch) in chunks.iter().enumerate() {
            acc += ch.len();
            if i + 1 < chunks.len() {
                println!("    boundary {} @ {:.2}s  RMS {:.4}", i + 1, acc as f32 / SR as f32, rms_at(acc));
            }
        }

        let _ = std::fs::remove_file(&chunk_mp3);
        let _ = std::fs::remove_file(&chunk_pcm);
    }
}
