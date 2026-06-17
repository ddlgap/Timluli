//! Acoustic speaker-gender classification by fundamental frequency (F0), V1.
//!
//! Hebrew is a gendered language: translating subtitles without knowing the
//! speaker's gender yields masculine-by-default inflections. This module tags
//! each subtitle cue `Male` / `Female` / `Unknown` from the pitch of the voice
//! (typical male median ≈85–155 Hz, female ≈165–255 Hz), entirely in-process —
//! no model download, no new dependency. The result is written as a time-keyed
//! `<stem>.genders.json` sidecar next to the generated SRT (time-keyed because
//! `srt::build_srt` splits/renumbers cues and users may edit the SRT — same
//! rationale as `video_transcription::words`), and consumed by the subtitle
//! translation path, which prefixes tagged cues with `[M]`/`[F]` for the LLM.
//!
//! Guiding principle ("do no harm"): a wrong tag is worse than no tag — the
//! masculine/neutral status quo is the fallback. Every gate below (voiced
//! ratio, cumulative voiced duration, the 155–175 Hz overlap dead-zone,
//! conservative smoothing) errs toward `Unknown`. Children classify as
//! `Female` (high F0) — a documented V1 limitation; diarization is V2.
//!
//! Pitch detection is a self-contained YIN implementation (de Cheveigné &
//! Kawahara 2002): per-frame cumulative-mean-normalized difference function,
//! absolute threshold, parabolic interpolation. The median over voiced frames
//! (never the mean) absorbs octave jumps and outliers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Sample rate the whole pipeline works in (ffmpeg extracts 16 kHz mono).
const SR: usize = 16_000;

/// Analysis frame: 1024 samples = 64 ms — ≥4 periods of the lowest F0 we track.
const FRAME: usize = 1024;
/// Plausible human F0 range; detections outside are treated as unvoiced.
const F0_MIN_HZ: f32 = 60.0;
const F0_MAX_HZ: f32 = 400.0;
/// YIN absolute threshold on the CMNDF dip. 0.15 is the paper's sweet spot:
/// low enough to reject noise/music frames, high enough to keep real voicing.
const YIN_THRESHOLD: f32 = 0.15;
/// Frames quieter than this RMS are silence — skip before YIN (avoids 0/0).
const RMS_FLOOR: f32 = 0.005;
/// Work cap per cue: at most this many frames, spread evenly over the window.
/// ~80 median samples are statistically plenty; keeps a full movie in seconds.
const MAX_FRAMES_PER_CUE: usize = 80;

/// Classification thresholds. The 155–175 Hz gap is a deliberate dead-zone —
/// the male/female overlap region maps to `Unknown`, never to a guess.
const MALE_MAX_HZ: f32 = 155.0;
const FEMALE_MIN_HZ: f32 = 175.0;
/// Minimum fraction of frames that must be voiced (music/noise/silence gate).
const MIN_VOICED_RATIO: f32 = 0.3;
/// Minimum estimated cumulative voiced duration in the cue, ms.
const MIN_VOICED_MS: f32 = 400.0;

/// Smoothing: an `Unknown` cue inherits its neighbors' shared label only when
/// both gaps to them are at most this (centiseconds) — speaker continuity.
const SMOOTH_MAX_GAP_CS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentGender {
    Male,
    Female,
    Unknown,
}

/// One cue's classification, carrying its time window (centiseconds, like the
/// rest of the crate) so the sidecar and smoothing can reason about gaps.
#[derive(Debug, Clone)]
pub struct CueGender {
    pub t0_cs: i64,
    pub t1_cs: i64,
    pub gender: SegmentGender,
}

// ─── YIN pitch detection ─────────────────────────────────────────────────────

/// Estimates F0 of one frame via YIN. `None` = unvoiced (no CMNDF dip under
/// the threshold inside the human F0 range).
fn yin_f0(frame: &[f32]) -> Option<f32> {
    let n = frame.len();
    let tau_min = (SR as f32 / F0_MAX_HZ) as usize; // ≈40
    let tau_max = ((SR as f32 / F0_MIN_HZ) as usize).min(n / 2); // ≈266
    if tau_max <= tau_min {
        return None;
    }

    // Difference function d(tau), then its cumulative-mean normalization.
    let mut cmndf = vec![1.0f32; tau_max + 1];
    let mut running_sum = 0.0f32;
    for tau in 1..=tau_max {
        let mut d = 0.0f32;
        for i in 0..(n - tau) {
            let diff = frame[i] - frame[i + tau];
            d += diff * diff;
        }
        running_sum += d;
        cmndf[tau] = if running_sum > 0.0 {
            d * tau as f32 / running_sum
        } else {
            1.0
        };
    }

    // Absolute threshold: first dip under YIN_THRESHOLD, walked to its local
    // minimum (avoids picking the shoulder of the dip).
    let mut tau = tau_min;
    while tau <= tau_max {
        if cmndf[tau] < YIN_THRESHOLD {
            while tau < tau_max && cmndf[tau + 1] < cmndf[tau] {
                tau += 1;
            }
            let refined = parabolic_min(&cmndf, tau, tau_max);
            let f0 = SR as f32 / refined;
            return (F0_MIN_HZ..=F0_MAX_HZ).contains(&f0).then_some(f0);
        }
        tau += 1;
    }
    None
}

/// Parabolic interpolation around a discrete minimum for sub-sample lag
/// precision (~1 Hz around the classification thresholds).
fn parabolic_min(v: &[f32], tau: usize, tau_max: usize) -> f32 {
    if tau == 0 || tau >= tau_max {
        return tau as f32;
    }
    let (a, b, c) = (v[tau - 1], v[tau], v[tau + 1]);
    let denom = a - 2.0 * b + c;
    if denom.abs() < f32::EPSILON {
        return tau as f32;
    }
    let shift = 0.5 * (a - c) / denom;
    tau as f32 + shift.clamp(-1.0, 1.0)
}

// ─── per-cue analysis ────────────────────────────────────────────────────────

/// Median F0 + voiced ratio of the sample window `[t0_cs, t1_cs]`.
/// Returns `(median_f0, voiced_ratio)`; `median_f0 = None` when nothing voiced.
fn analyze_window(samples: &[f32], t0_cs: i64, t1_cs: i64) -> (Option<f32>, f32) {
    let start = ((t0_cs.max(0) as usize) * SR / 100).min(samples.len());
    let end = ((t1_cs.max(0) as usize) * SR / 100).min(samples.len());
    let window = &samples[start..end];
    if window.len() < FRAME {
        return (None, 0.0);
    }

    // 50% overlap by default; widen the hop when the cue would exceed the
    // per-cue frame cap (evenly-spread subsampling — the median doesn't care).
    let natural_frames = (window.len() - FRAME) / (FRAME / 2) + 1;
    let hop = if natural_frames > MAX_FRAMES_PER_CUE {
        (window.len() - FRAME) / (MAX_FRAMES_PER_CUE - 1)
    } else {
        FRAME / 2
    }
    .max(1);

    let mut f0s: Vec<f32> = Vec::new();
    let mut total = 0usize;
    let mut pos = 0usize;
    while pos + FRAME <= window.len() {
        let frame = &window[pos..pos + FRAME];
        total += 1;
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        if rms >= RMS_FLOOR {
            if let Some(f0) = yin_f0(frame) {
                f0s.push(f0);
            }
        }
        pos += hop;
    }
    if total == 0 {
        return (None, 0.0);
    }

    let voiced_ratio = f0s.len() as f32 / total as f32;
    if f0s.is_empty() {
        return (None, voiced_ratio);
    }
    f0s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if f0s.len() % 2 == 1 {
        f0s[f0s.len() / 2]
    } else {
        (f0s[f0s.len() / 2 - 1] + f0s[f0s.len() / 2]) / 2.0
    };
    (Some(median), voiced_ratio)
}

/// Classifies one analyzed window. Every gate falls through to `Unknown`.
fn classify(median_f0: Option<f32>, voiced_ratio: f32, dur_ms: f32) -> SegmentGender {
    let Some(f0) = median_f0 else {
        return SegmentGender::Unknown;
    };
    // The frames are an even subsample of the cue, so the voiced ratio is an
    // unbiased estimator of the voiced fraction of its duration.
    let voiced_ms = voiced_ratio * dur_ms;
    if voiced_ratio < MIN_VOICED_RATIO || voiced_ms < MIN_VOICED_MS {
        return SegmentGender::Unknown;
    }
    if f0 < MALE_MAX_HZ {
        SegmentGender::Male
    } else if f0 > FEMALE_MIN_HZ {
        SegmentGender::Female
    } else {
        SegmentGender::Unknown
    }
}

/// Classifies every cue window over the shared 16 kHz mono PCM. Pure CPU work —
/// run it on a blocking thread for long inputs.
pub fn classify_cues(samples: &[f32], windows: &[(i64, i64)]) -> Vec<CueGender> {
    windows
        .iter()
        .map(|&(t0_cs, t1_cs)| {
            let (median, voiced_ratio) = analyze_window(samples, t0_cs, t1_cs);
            let dur_ms = ((t1_cs - t0_cs).max(0) * 10) as f32;
            CueGender {
                t0_cs,
                t1_cs,
                gender: classify(median, voiced_ratio, dur_ms),
            }
        })
        .collect()
}

/// Conservative continuity smoothing: an `Unknown` cue sandwiched between two
/// cues that share a label, with both gaps ≤ ~3 s, inherits that label.
/// Reads the *original* labels (not freshly-inherited ones) so confidence can
/// only be added, never cascaded or flipped.
pub fn smooth(cues: &mut [CueGender]) {
    if cues.len() < 3 {
        return;
    }
    let original: Vec<SegmentGender> = cues.iter().map(|c| c.gender).collect();
    for i in 1..cues.len() - 1 {
        if original[i] != SegmentGender::Unknown {
            continue;
        }
        let prev = original[i - 1];
        if prev == SegmentGender::Unknown || prev != original[i + 1] {
            continue;
        }
        let gap_before = cues[i].t0_cs - cues[i - 1].t1_cs;
        let gap_after = cues[i + 1].t0_cs - cues[i].t1_cs;
        if gap_before <= SMOOTH_MAX_GAP_CS && gap_after <= SMOOTH_MAX_GAP_CS {
            cues[i].gender = prev;
        }
    }
}

// ─── sidecar (`<stem>.genders.json`) ─────────────────────────────────────────

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct GendersFile {
    version: u32,
    cues: Vec<GenderEntry>,
}

/// Times are seconds in the file (like `words.json`); centiseconds in-process.
#[derive(Serialize, Deserialize)]
struct GenderEntry {
    t0: f64,
    t1: f64,
    g: String,
}

/// `movie.srt` → `movie.genders.json` (same directory, same stem).
pub fn sidecar_path(srt_path: &Path) -> PathBuf {
    let stem = srt_path.file_stem().and_then(|s| s.to_str()).unwrap_or("video");
    srt_path.with_file_name(format!("{stem}.genders.json"))
}

/// Writes the sidecar with only the labeled (M/F) cues. Returns `Ok(false)`
/// without writing when nothing is labeled — no file is better than an empty one.
pub fn write_sidecar(path: &Path, cues: &[CueGender]) -> Result<bool, String> {
    let entries: Vec<GenderEntry> = cues
        .iter()
        .filter_map(|c| {
            let g = match c.gender {
                SegmentGender::Male => "M",
                SegmentGender::Female => "F",
                SegmentGender::Unknown => return None,
            };
            Some(GenderEntry {
                t0: c.t0_cs as f64 / 100.0,
                t1: c.t1_cs as f64 / 100.0,
                g: g.to_string(),
            })
        })
        .collect();
    if entries.is_empty() {
        return Ok(false);
    }
    let json = serde_json::to_string(&GendersFile { version: VERSION, cues: entries })
        .map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Loads the sidecar matching `srt_path`. Any problem — missing file, bad
/// JSON, unknown version, no entries — yields `None`: translation then simply
/// runs untagged, exactly like today.
pub fn load_for_srt(srt_path: &Path) -> Option<Vec<CueGender>> {
    let raw = std::fs::read_to_string(sidecar_path(srt_path)).ok()?;
    let parsed: GendersFile = serde_json::from_str(&raw).ok()?;
    if parsed.version != VERSION {
        return None;
    }
    let cues: Vec<CueGender> = parsed
        .cues
        .into_iter()
        .filter_map(|e| {
            if !e.t0.is_finite() || !e.t1.is_finite() {
                return None;
            }
            let gender = match e.g.as_str() {
                "M" => SegmentGender::Male,
                "F" => SegmentGender::Female,
                _ => return None,
            };
            let t0_cs = (e.t0.max(0.0) * 100.0).round() as i64;
            let t1_cs = (e.t1.max(0.0) * 100.0).round() as i64;
            Some(CueGender { t0_cs, t1_cs: t1_cs.max(t0_cs), gender })
        })
        .collect();
    (!cues.is_empty()).then_some(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of a pure sine at `hz`, 16 kHz, amplitude 0.5.
    fn sine(hz: f32, secs: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / SR as f32).sin())
            .collect()
    }

    /// Deterministic white noise (LCG), amplitude ~±0.5.
    fn white_noise(secs: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        let mut state: u32 = 0x1234_5678;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect()
    }

    fn classify_one(samples: &[f32], t0_cs: i64, t1_cs: i64) -> SegmentGender {
        classify_cues(samples, &[(t0_cs, t1_cs)])[0].gender
    }

    #[test]
    fn sine_110hz_is_male_220hz_is_female() {
        let male = sine(110.0, 2.0);
        assert_eq!(classify_one(&male, 0, 200), SegmentGender::Male);
        let female = sine(220.0, 2.0);
        assert_eq!(classify_one(&female, 0, 200), SegmentGender::Female);
    }

    #[test]
    fn overlap_zone_165hz_is_unknown() {
        let ambiguous = sine(165.0, 2.0);
        assert_eq!(classify_one(&ambiguous, 0, 200), SegmentGender::Unknown);
    }

    #[test]
    fn silence_and_white_noise_are_unknown() {
        let silence = vec![0.0f32; 2 * SR];
        assert_eq!(classify_one(&silence, 0, 200), SegmentGender::Unknown);
        let noise = white_noise(2.0);
        assert_eq!(classify_one(&noise, 0, 200), SegmentGender::Unknown);
    }

    #[test]
    fn too_short_voiced_is_unknown() {
        // 300 ms of clean 110 Hz — under the 400 ms cumulative-voicing floor.
        let short = sine(110.0, 0.3);
        assert_eq!(classify_one(&short, 0, 30), SegmentGender::Unknown);
    }

    #[test]
    fn yin_frequency_accuracy() {
        for hz in [85.0f32, 120.0, 155.0, 200.0, 250.0, 320.0] {
            let s = sine(hz, 0.1);
            let f0 = yin_f0(&s[..FRAME]).unwrap_or_else(|| panic!("{hz} Hz: unvoiced"));
            assert!((f0 - hz).abs() < 3.0, "{hz} Hz detected as {f0} Hz");
        }
    }

    fn cue(t0_cs: i64, t1_cs: i64, gender: SegmentGender) -> CueGender {
        CueGender { t0_cs, t1_cs, gender }
    }

    #[test]
    fn smoothing_fills_unknown_between_same_gender_close_neighbors() {
        let mut cues = vec![
            cue(0, 100, SegmentGender::Female),
            cue(150, 250, SegmentGender::Unknown),
            cue(300, 400, SegmentGender::Female),
        ];
        smooth(&mut cues);
        assert_eq!(cues[1].gender, SegmentGender::Female);
    }

    #[test]
    fn smoothing_leaves_unknown_between_different_genders() {
        let mut cues = vec![
            cue(0, 100, SegmentGender::Female),
            cue(150, 250, SegmentGender::Unknown),
            cue(300, 400, SegmentGender::Male),
        ];
        smooth(&mut cues);
        assert_eq!(cues[1].gender, SegmentGender::Unknown);
    }

    #[test]
    fn smoothing_respects_time_gap_and_never_flips_labels() {
        // Gap of 5 s after the unknown — too far for speaker continuity.
        let mut far = vec![
            cue(0, 100, SegmentGender::Male),
            cue(150, 250, SegmentGender::Unknown),
            cue(750, 850, SegmentGender::Male),
        ];
        smooth(&mut far);
        assert_eq!(far[1].gender, SegmentGender::Unknown);

        // A confident label between opposite neighbors must never flip.
        let mut confident = vec![
            cue(0, 100, SegmentGender::Female),
            cue(150, 250, SegmentGender::Male),
            cue(300, 400, SegmentGender::Female),
        ];
        smooth(&mut confident);
        assert_eq!(confident[1].gender, SegmentGender::Male);
    }

    #[test]
    fn smoothing_does_not_cascade_inherited_labels() {
        // F, U, U, F: neither U has two originally-labeled same-gender
        // neighbors, so neither inherits (reads originals, not results).
        let mut cues = vec![
            cue(0, 100, SegmentGender::Female),
            cue(120, 220, SegmentGender::Unknown),
            cue(240, 340, SegmentGender::Unknown),
            cue(360, 460, SegmentGender::Female),
        ];
        smooth(&mut cues);
        assert_eq!(cues[1].gender, SegmentGender::Unknown);
        assert_eq!(cues[2].gender, SegmentGender::Unknown);
    }

    #[test]
    fn sidecar_roundtrip_and_unknown_only_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("tl_genders_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("סרט.srt");

        let cues = vec![
            cue(0, 250, SegmentGender::Male),
            cue(300, 500, SegmentGender::Unknown),
            cue(550, 800, SegmentGender::Female),
        ];
        assert!(write_sidecar(&sidecar_path(&srt), &cues).unwrap());
        assert!(dir.join("סרט.genders.json").exists());

        let loaded = load_for_srt(&srt).expect("load");
        assert_eq!(loaded.len(), 2, "Unknown cues are not persisted");
        assert_eq!(loaded[0].gender, SegmentGender::Male);
        assert_eq!((loaded[0].t0_cs, loaded[0].t1_cs), (0, 250));
        assert_eq!(loaded[1].gender, SegmentGender::Female);
        assert_eq!((loaded[1].t0_cs, loaded[1].t1_cs), (550, 800));

        // All-Unknown input: nothing written, and a previous file would be left
        // alone (the pipeline removes stale sidecars before analysis instead).
        let all_unknown = vec![cue(0, 100, SegmentGender::Unknown)];
        let p2 = dir.join("ריק.genders.json");
        assert!(!write_sidecar(&p2, &all_unknown).unwrap());
        assert!(!p2.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_load_rejects_garbage() {
        let dir = std::env::temp_dir().join(format!("tl_genders_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("x.srt");
        assert!(load_for_srt(&srt).is_none(), "missing file");
        std::fs::write(dir.join("x.genders.json"), "not json").unwrap();
        assert!(load_for_srt(&srt).is_none(), "malformed json");
        std::fs::write(
            dir.join("x.genders.json"),
            r#"{"version":99,"cues":[{"t0":0,"t1":1,"g":"M"}]}"#,
        )
        .unwrap();
        assert!(load_for_srt(&srt).is_none(), "unknown version");
        std::fs::write(
            dir.join("x.genders.json"),
            r#"{"version":1,"cues":[{"t0":0,"t1":1,"g":"X"}]}"#,
        )
        .unwrap();
        assert!(load_for_srt(&srt).is_none(), "unknown gender letter");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads a 16 kHz mono PCM-s16LE WAV into f32 [-1,1]. Locates the `data` chunk
    /// so a non-44-byte header still works.
    fn read_wav_16k_mono(path: &str) -> Vec<f32> {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let data_pos = bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("no `data` chunk in WAV");
        bytes[data_pos + 8..] // skip "data" tag (4) + chunk size (4)
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect()
    }

    /// Baselines the EXISTING F0 classifier on the canonical labeled clip
    /// (`michael_21-24.wav`, 16 kHz mono). Prints a 3 s median-F0 + M/F timeline and
    /// a per-ground-truth-segment verdict so we can compare against the user's labels
    /// (0:22–0:44 two boys=M; 0:44–1:11 two men=M; 1:13–3:00 boy=M + mother=F).
    /// `#[ignore]`d — needs the clip on disk:
    ///   cargo test --lib f0_baseline_on_clip -- --ignored --nocapture
    #[test]
    #[ignore]
    fn f0_baseline_on_clip() {
        let path = std::env::var("TIMLULI_TEST_CLIP")
            .unwrap_or_else(|_| r"C:\Users\Lenovo\Desktop\gender-clips\michael_21-24.wav".to_string());
        let samples = read_wav_16k_mono(&path);
        let total_cs = (samples.len() * 100 / SR) as i64;
        println!("\nclip: {} samples = {:.1}s\n", samples.len(), samples.len() as f32 / SR as f32);

        println!("=== 3 s sweep (median F0 / voiced ratio / gender) ===");
        let win = 300i64;
        let mut t = 0i64;
        while t < total_cs {
            let end = (t + win).min(total_cs);
            let (f0, vr) = analyze_window(&samples, t, end);
            let g = classify(f0, vr, ((end - t) * 10) as f32);
            println!(
                "  {:3}-{:3}s  {:>7}  vr={:4.2}  {:?}",
                t / 100,
                end / 100,
                f0.map(|v| format!("{v:.0}Hz")).unwrap_or_else(|| "-".into()),
                vr,
                g
            );
            t += win;
        }

        println!("\n=== per ground-truth segment ===");
        for (a, b, who, expect) in [
            (22i64, 44i64, "two boys", "M"),
            (44, 71, "two men", "M"),
            (73, 180, "boy + mother", "M/F"),
        ] {
            let (f0, vr) = analyze_window(&samples, a * 100, b * 100);
            let g = classify(f0, vr, ((b - a) * 1000) as f32);
            println!(
                "  {a:3}-{b:3}s  {who:14} expect {expect:3}  medianF0={:>7}  vr={vr:4.2}  -> {:?}",
                f0.map(|v| format!("{v:.0}Hz")).unwrap_or_else(|| "-".into()),
                g
            );
        }
    }

    /// Groups above-floor 64 ms frames into speech regions (cs), bridging gaps
    /// < 0.3 s, keeping regions >= 0.6 s — utterance-level windows like a VAD/whisper
    /// cue, so `analyze_window` sees speech-dense windows (unlike blind fixed windows
    /// diluted by inter-line silence/music).
    fn detect_speech_regions(samples: &[f32], floor: f32) -> Vec<(i64, i64)> {
        let frame_cs = (FRAME as i64 * 100) / SR as i64;
        let max_gap_frames = 30 / frame_cs.max(1);
        let min_len_cs = 60i64;
        let mut regions = Vec::new();
        let mut cur_start: Option<i64> = None;
        let mut last_voiced = -999i64;
        let mut fi = 0i64;
        let mut i = 0;
        while i + FRAME <= samples.len() {
            let rms = (samples[i..i + FRAME].iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
            if rms > floor {
                if cur_start.is_none() {
                    cur_start = Some(fi);
                }
                last_voiced = fi;
            } else if let Some(start) = cur_start {
                if fi - last_voiced > max_gap_frames {
                    let (s_cs, e_cs) = (start * frame_cs, (last_voiced + 1) * frame_cs);
                    if e_cs - s_cs >= min_len_cs {
                        regions.push((s_cs, e_cs));
                    }
                    cur_start = None;
                }
            }
            fi += 1;
            i += FRAME;
        }
        if let Some(start) = cur_start {
            let (s_cs, e_cs) = (start * frame_cs, (last_voiced + 1) * frame_cs);
            if e_cs - s_cs >= min_len_cs {
                regions.push((s_cs, e_cs));
            }
        }
        regions
    }

    /// Decisive test: F0 gender on SPEECH-ALIGNED regions (not blind windows), showing
    /// current-gate vs a film-relaxed gate (vr>=0.15) per region with its ground-truth
    /// zone. Answers whether adults (men->M, mother->F) get tagged once windows are
    /// speech-aligned, and confirms boys->F (wrong, fundamental). `#[ignore]`d:
    ///   cargo test --lib f0_speech_regions_on_clip -- --ignored --nocapture
    #[test]
    #[ignore]
    fn f0_speech_regions_on_clip() {
        let path = std::env::var("TIMLULI_TEST_CLIP")
            .unwrap_or_else(|_| r"C:\Users\Lenovo\Desktop\gender-clips\michael_21-24.wav".to_string());
        let samples = read_wav_16k_mono(&path);
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
        let peak = samples.iter().fold(0f32, |m, &s| m.max(s.abs()));
        let floor = (0.6 * rms).max(0.005);
        println!(
            "\nlevel: rms={rms:.4} ({:.1} dBFS), peak={peak:.3}; speech floor={floor:.4}",
            20.0 * rms.max(1e-9).log10()
        );
        let regions = detect_speech_regions(&samples, floor);
        let g_str = |g: SegmentGender| match g {
            SegmentGender::Male => "Male",
            SegmentGender::Female => "Female",
            SegmentGender::Unknown => "Unknown",
        };
        let zone = |t_cs: i64| -> &'static str {
            let s = t_cs / 100;
            if (22..44).contains(&s) {
                "boys(exp M)"
            } else if (44..71).contains(&s) {
                "men(exp M)"
            } else if (73..=180).contains(&s) {
                "boy+mother(M/F)"
            } else {
                "-"
            }
        };
        println!("\n{} speech regions\n", regions.len());
        println!("  region       dur  f0       vr    current   relaxed   zone");
        for (s, e) in &regions {
            let (f0, vr) = analyze_window(&samples, *s, *e);
            let dur_ms = ((e - s) * 10) as f32;
            let cur = classify(f0, vr, dur_ms);
            let relaxed = match f0 {
                Some(hz) if vr >= 0.15 && vr * dur_ms >= 400.0 => {
                    if hz < MALE_MAX_HZ {
                        SegmentGender::Male
                    } else if hz > FEMALE_MIN_HZ {
                        SegmentGender::Female
                    } else {
                        SegmentGender::Unknown
                    }
                }
                _ => SegmentGender::Unknown,
            };
            println!(
                "  {:3}-{:3}s  {:3}s  {:>6}  {:4.2}  {:8}  {:8}  {}",
                s / 100,
                e / 100,
                (e - s) / 100,
                f0.map(|v| format!("{v:.0}Hz")).unwrap_or_else(|| "-".into()),
                vr,
                g_str(cur),
                g_str(relaxed),
                zone((s + e) / 2)
            );
        }
    }
}
