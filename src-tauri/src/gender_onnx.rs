//! In-process acoustic gender classifier (V2): a Wav2Vec2 sequence-classification
//! model (Common-Voice fine-tune, Apache-2.0) run via `ort` (load-dynamic → the same
//! bundled `onnxruntime.dll` as the punctuation engine; no new dependency, no Python).
//! Raw 16 kHz waveform in → `female`/`male` logits out.
//!
//! This is an OPTIONAL ~95 MB download that augments the always-available F0 path in
//! [`crate::gender_f0`]. When loaded, the video pipeline keeps the model's label for a
//! cue ONLY when it is confident (≥ [`ACCEPT_CONFIDENCE`]); otherwise the conservative
//! F0 label stands. This fixes exactly the cases F0 misses — adult males that YIN
//! octave-doubles into the female range, and the 155–175 Hz overlap dead-zone — while
//! preserving "do no harm": silence/music cues are gated out and fall back to F0.
//!
//! Validated on a labeled clip (see project memory / `Desktop\gender-onnx-poc`): two
//! men → Male @0.999 (F0 read ~246 Hz → wrong), mother → Female @0.996. Children stay
//! a fundamental limit — they classify Female, exactly as the F0 path already does.
//!
//! `GenderEngineHandle` mirrors `PunctuationEngineHandle`: a `parking_lot::Mutex` so a
//! batch (the whole movie's cues) locks once and runs on a `spawn_blocking` thread.

use std::path::Path;
use std::sync::Arc;

use ort::session::Session;
use ort::value::Value;
use parking_lot::Mutex;

use crate::gender_f0::SegmentGender;

/// Pipeline sample rate (ffmpeg extracts 16 kHz mono).
const SR: usize = 16_000;
/// Windows quieter than this RMS are silence/near-silence — skip them: the model's
/// per-window normalization would otherwise amplify noise into a confident, wrong
/// label. Matches `gender_f0::RMS_FLOOR`.
const RMS_FLOOR: f32 = 0.005;
/// Cue audio shorter than this is too little to trust (0.5 s).
const MIN_SAMPLES: usize = SR / 2;
/// Cap the audio fed per cue (center-cropped): bounds CPU on long cues; a few seconds
/// is ample for a gender decision and matches the model's training clip length.
const MAX_SAMPLES: usize = 8 * SR;

/// Minimum softmax confidence for the model's label to OVERRIDE the F0 guess. Below
/// this, the conservative F0 label (which may be `Unknown`) stands. 0.85 cleared the
/// validation clip's adult cases at ≥0.99 while leaving room above coin-flip noise.
pub const ACCEPT_CONFIDENCE: f32 = 0.85;

// The optimum ONNX export of `Wav2Vec2ForSequenceClassification` has one float input
// `input_values` [batch, samples] and one output `logits` [batch, 2] = (female, male).

pub struct GenderEngine {
    session: Session,
}

impl GenderEngine {
    /// Loads the ONNX model. `onnxruntime.dll` is registered once via the shared
    /// [`crate::onnx_runtime::init`] (the same runtime the punctuation engine uses).
    pub fn load(model_path: &Path) -> Result<Self, String> {
        crate::onnx_runtime::init()?;
        let session = Session::builder()
            .map_err(|e| format!("ort builder: {e}"))?
            .with_intra_threads(num_cpus::get().min(4))
            .map_err(|e| format!("ort threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("טעינת מודל המגדר נכשלה: {e}"))?;
        Ok(Self { session })
    }

    /// Classifies one cue's mono samples → `(gender, confidence)`, or `None` when the
    /// window is too short/quiet or inference fails (caller keeps the F0 label).
    pub fn classify(&mut self, samples: &[f32]) -> Option<(SegmentGender, f32)> {
        if samples.len() < MIN_SAMPLES {
            return None;
        }
        // Center-crop overly long cues — the decision needs only a few seconds.
        let win = if samples.len() > MAX_SAMPLES {
            let start = (samples.len() - MAX_SAMPLES) / 2;
            &samples[start..start + MAX_SAMPLES]
        } else {
            samples
        };

        // Energy gate BEFORE normalization (so silence stays silence).
        let mean = win.iter().sum::<f32>() / win.len() as f32;
        let var = win.iter().map(|&s| (s - mean) * (s - mean)).sum::<f32>() / win.len() as f32;
        if var.sqrt() < RMS_FLOOR {
            return None;
        }

        // Wav2Vec2FeatureExtractor `do_normalize`: zero-mean, unit-variance.
        let inv = 1.0 / (var + 1e-7).sqrt();
        let norm: Vec<f32> = win.iter().map(|&s| (s - mean) * inv).collect();
        let n = norm.len();

        let input = Value::from_array(([1_usize, n], norm)).ok()?;
        let outputs = self.session.run(ort::inputs!["input_values" => input]).ok()?;
        let (_shape, logits) = outputs["logits"].try_extract_tensor::<f32>().ok()?;
        if logits.len() < 2 {
            return None;
        }

        // softmax over [female, male].
        let (f, m) = (logits[0], logits[1]);
        let mx = f.max(m);
        let (ef, em) = ((f - mx).exp(), (m - mx).exp());
        let sum = ef + em;
        if !sum.is_finite() || sum <= 0.0 {
            return None;
        }
        let (pf, pm) = (ef / sum, em / sum);
        if pm >= pf {
            Some((SegmentGender::Male, pm))
        } else {
            Some((SegmentGender::Female, pf))
        }
    }
}

/// Loaded-engine handle stored in `AppState` (None = off / not installed). The inner
/// `parking_lot::Mutex` serializes inference; a whole-movie batch locks it once.
pub struct GenderEngineHandle {
    engine: Arc<Mutex<GenderEngine>>,
}

impl GenderEngineHandle {
    pub fn load(model_path: &Path) -> Result<Self, String> {
        Ok(Self {
            engine: Arc::new(Mutex::new(GenderEngine::load(model_path)?)),
        })
    }

    /// Classifies each cue window over the shared 16 kHz PCM. Returns one
    /// `Option<(gender, confidence)>` per window, aligned by index. Blocking —
    /// call inside `spawn_blocking`.
    pub fn classify_windows(
        &self,
        samples: &[f32],
        windows: &[(i64, i64)],
    ) -> Vec<Option<(SegmentGender, f32)>> {
        let mut eng = self.engine.lock();
        windows
            .iter()
            .map(|&(t0_cs, t1_cs)| {
                let start = ((t0_cs.max(0) as usize) * SR / 100).min(samples.len());
                let end = ((t1_cs.max(0) as usize) * SR / 100).min(samples.len());
                if end <= start {
                    return None;
                }
                eng.classify(&samples[start..end])
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a 16 kHz mono PCM-s16LE WAV into f32 [-1,1] (locates the `data` chunk).
    fn read_wav_16k_mono(path: &str) -> Vec<f32> {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let data_pos = bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("no `data` chunk in WAV");
        bytes[data_pos + 8..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect()
    }

    /// End-to-end check of the REAL engine code (normalize → softmax → energy gate)
    /// AND the video-pipeline ensemble rule (model overrides F0 when conf ≥ accept),
    /// on the canonical labeled clip. Needs the model + onnxruntime.dll next to the
    /// test exe — not in CI. Run:
    ///   cargo test --lib gender_onnx_on_clip -- --ignored --nocapture
    /// Ground truth (clip seconds): 22-44 boys=M, 44-71 two men=M, 73-180 boy+mother;
    /// the clean ADULT cases to validate are MEN (44-71 → Male) and the mother's solid
    /// run (~97-163 → Female). Boys → Female is the accepted fundamental limit.
    #[test]
    #[ignore]
    fn gender_onnx_on_clip() {
        let model = std::env::var("TIMLULI_GENDER_MODEL").unwrap_or_else(|_| {
            r"C:\Users\Lenovo\Desktop\gender-onnx-poc\model_quantized.onnx".to_string()
        });
        let clip = std::env::var("TIMLULI_TEST_CLIP").unwrap_or_else(|_| {
            r"C:\Users\Lenovo\Desktop\gender-clips\michael_21-24.wav".to_string()
        });
        let samples = read_wav_16k_mono(&clip);
        println!("\nclip {:.1}s | model {}", samples.len() as f32 / SR as f32, model);
        let handle = GenderEngineHandle::load(std::path::Path::new(&model)).expect("load model");

        // 4 s / 2 s sweep windows (centiseconds), like the validation POC.
        let (win_cs, hop_cs) = (400i64, 200i64);
        let total_cs = (samples.len() * 100 / SR) as i64;
        let mut windows = Vec::new();
        let mut t = 0i64;
        while t + win_cs <= total_cs {
            windows.push((t, t + win_cs));
            t += hop_cs;
        }

        // The real engine path (what the video pipeline calls).
        let onnx = handle.classify_windows(&samples, &windows);
        // The F0 path + the exact ensemble-override rule from video_transcription.
        let mut ens = crate::gender_f0::classify_cues(&samples, &windows);
        for (c, o) in ens.iter_mut().zip(&onnx) {
            if let Some((g, conf)) = o {
                if *conf >= ACCEPT_CONFIDENCE {
                    c.gender = *g;
                }
            }
        }

        let zone = |cs: i64| -> &'static str {
            let s = cs / 100;
            if (22..44).contains(&s) {
                "boys(expM)"
            } else if (44..71).contains(&s) {
                "MEN(expM)"
            } else if (97..=163).contains(&s) {
                "mother(expF)"
            } else if (73..=180).contains(&s) {
                "mom+boy"
            } else {
                "-"
            }
        };
        let g_str = |g: SegmentGender| match g {
            SegmentGender::Male => "Male",
            SegmentGender::Female => "Female",
            SegmentGender::Unknown => "Unknown",
        };

        // Per-zone tally of the ONNX verdict + a few sample rows.
        let (mut men_m, mut men_f) = (0u32, 0u32);
        let (mut mom_m, mut mom_f) = (0u32, 0u32);
        println!("\n  time   zone          F0        ONNX(conf)     ensemble");
        for (i, &(a, _b)) in windows.iter().enumerate() {
            let z = zone(a);
            let o = onnx[i];
            let f0 = crate::gender_f0::classify_cues(&samples, &[windows[i]])[0].gender;
            if z == "MEN(expM)" {
                match o.map(|x| x.0) {
                    Some(SegmentGender::Male) => men_m += 1,
                    Some(SegmentGender::Female) => men_f += 1,
                    _ => {}
                }
            }
            if z == "mother(expF)" {
                match o.map(|x| x.0) {
                    Some(SegmentGender::Male) => mom_m += 1,
                    Some(SegmentGender::Female) => mom_f += 1,
                    _ => {}
                }
            }
            if z != "-" && i % 3 == 0 {
                let os = o
                    .map(|(g, c)| format!("{}({:.2})", g_str(g), c))
                    .unwrap_or_else(|| "—".into());
                println!(
                    "  {:4}s  {:12}  {:8}  {:14} {}",
                    a / 100,
                    z,
                    g_str(f0),
                    os,
                    g_str(ens[i].gender)
                );
            }
        }
        println!("\nMEN zone   ONNX: Male={men_m} Female={men_f}");
        println!("MOTHER zone ONNX: Male={mom_m} Female={mom_f}");

        // The decisive adult cases F0 misses must come out right via the real code.
        assert!(men_m > men_f, "two-men region should be majority Male (got M={men_m} F={men_f})");
        assert!(mom_f > mom_m, "mother region should be majority Female (got M={mom_m} F={mom_f})");
    }

    /// End-to-end on the AUDIO SAMPLE through the EXACT shipping ensemble: real Groq
    /// transcript cues + F0 + ONNX(conf≥0.85) + text(grammar/name) override + smooth.
    /// Shows the text layer's real-world firing (≈0 on this English clip whose names are
    /// vocative/3rd-person, NOT self-intro → correct do-no-harm) and the per-cue result.
    ///   cargo test --lib pipeline_on_clip_transcript -- --ignored --nocapture
    #[test]
    #[ignore]
    fn pipeline_on_clip_transcript() {
        fn gs(g: SegmentGender) -> &'static str {
            match g {
                SegmentGender::Male => "M",
                SegmentGender::Female => "F",
                SegmentGender::Unknown => "-",
            }
        }
        let model = std::env::var("TIMLULI_GENDER_MODEL").unwrap_or_else(|_| {
            r"C:\Users\Lenovo\Desktop\gender-onnx-poc\model_quantized.onnx".to_string()
        });
        let clip = std::env::var("TIMLULI_TEST_CLIP").unwrap_or_else(|_| {
            r"C:\Users\Lenovo\Desktop\gender-clips\michael_21-24.wav".to_string()
        });
        let tj = std::env::var("TIMLULI_TEST_TRANSCRIPT").unwrap_or_else(|_| {
            r"C:\Users\Lenovo\Desktop\gender-clips\michael_transcript.json".to_string()
        });
        let samples = read_wav_16k_mono(&clip);
        let raw = std::fs::read_to_string(&tj).expect("transcript json");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
        let segs = v["segments"].as_array().expect("segments array");
        let cues: Vec<(i64, i64, String)> = segs
            .iter()
            .filter_map(|s| {
                let t0 = (s["start"].as_f64()? * 100.0).round() as i64;
                let t1 = (s["end"].as_f64()? * 100.0).round() as i64;
                let txt = s["text"].as_str()?.trim().to_string();
                Some((t0, t1, txt))
            })
            .collect();
        let windows: Vec<(i64, i64)> = cues.iter().map(|(a, b, _)| (*a, *b)).collect();
        let handle = GenderEngineHandle::load(std::path::Path::new(&model)).expect("model");

        // EXACT shipping ensemble (mirrors video_transcription::mod).
        let mut cg = crate::gender_f0::classify_cues(&samples, &windows);
        let onnx = handle.classify_windows(&samples, &windows);
        for (c, o) in cg.iter_mut().zip(&onnx) {
            if let Some((g, conf)) = o {
                if *conf >= ACCEPT_CONFIDENCE {
                    c.gender = *g;
                }
            }
        }
        let mut text_fired = 0usize;
        for (c, (_, _, txt)) in cg.iter_mut().zip(&cues) {
            if let Some(g) = crate::gender_text::infer_speaker_gender(txt) {
                c.gender = g;
                text_fired += 1;
            }
        }
        crate::gender_f0::smooth(&mut cg);

        println!("\n{} cues | text-layer fired on {} cues\n", cues.len(), text_fired);
        for (i, (a, _, txt)) in cues.iter().enumerate() {
            let f0 = crate::gender_f0::classify_cues(&samples, &[windows[i]])[0].gender;
            let o = onnx[i]
                .map(|(g, c)| format!("{}{:.2}", gs(g), c))
                .unwrap_or_else(|| "-".into());
            let t = crate::gender_text::infer_speaker_gender(txt).map(gs).unwrap_or("-");
            let snip: String = txt.chars().take(46).collect();
            println!("{:4}s F0={} ONNX={:>6} TXT={} -> {}  {}", a / 100, gs(f0), o, t, gs(cg[i].gender), snip);
        }
        let m = cg.iter().filter(|c| c.gender == SegmentGender::Male).count();
        let f = cg.iter().filter(|c| c.gender == SegmentGender::Female).count();
        let u = cg.iter().filter(|c| c.gender == SegmentGender::Unknown).count();
        println!("\nFINAL: Male={m} Female={f} Unknown={u}");
    }
}
