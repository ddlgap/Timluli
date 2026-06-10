//! The `<stem>.words.json` sidecar: word-level timestamps saved next to a
//! generated SRT, consumed by the karaoke burn-in style (`subtitle_burn`).
//!
//! Deliberately **flat** — no `srt_index` linkage: `srt::build_srt` splits
//! segments into cues, so segment indices don't map to cue numbers, and the
//! user may hand-edit the SRT afterwards. The burn side matches words to cues
//! by time-overlap + normalized-text comparison instead, which survives both.
//!
//! Times are seconds in the file (mirroring the engines' native output);
//! in-process the crate currency is centiseconds, like everywhere else.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

/// One spoken word with absolute timing (centiseconds).
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub w: String,
    pub t0_cs: i64,
    pub t1_cs: i64,
}

#[derive(Serialize, Deserialize)]
struct WordsFile {
    version: u32,
    engine: String,
    language: String,
    words: Vec<WordEntry>,
}

#[derive(Serialize, Deserialize)]
struct WordEntry {
    w: String,
    t0: f64,
    t1: f64,
}

/// `movie.srt` → `movie.words.json` (same directory, same stem).
pub fn sidecar_path(srt_path: &Path) -> PathBuf {
    let stem = srt_path.file_stem().and_then(|s| s.to_str()).unwrap_or("video");
    srt_path.with_file_name(format!("{stem}.words.json"))
}

/// Serializes and writes the sidecar. Plain `Result` — the caller decides how
/// soft the failure is (the SRT pipeline treats it as best-effort).
pub fn write(path: &Path, engine: &str, words: &[TimedWord]) -> Result<(), String> {
    let file = WordsFile {
        version: VERSION,
        engine: engine.to_string(),
        language: "he".into(),
        words: words
            .iter()
            .map(|w| WordEntry {
                w: w.w.clone(),
                t0: w.t0_cs as f64 / 100.0,
                t1: w.t1_cs as f64 / 100.0,
            })
            .collect(),
    };
    let json = serde_json::to_string(&file).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Loads the sidecar matching `srt_path`. Any problem — missing file, bad JSON,
/// unknown version, empty list — returns `None`: the karaoke style then
/// degrades instead of failing the burn.
pub fn load_for_srt(srt_path: &Path) -> Option<Vec<TimedWord>> {
    let raw = std::fs::read_to_string(sidecar_path(srt_path)).ok()?;
    let parsed: WordsFile = serde_json::from_str(&raw).ok()?;
    if parsed.version != VERSION || parsed.words.is_empty() {
        return None;
    }
    let words: Vec<TimedWord> = parsed
        .words
        .into_iter()
        .filter_map(|e| {
            let w = e.w.trim().to_string();
            if w.is_empty() || !e.t0.is_finite() || !e.t1.is_finite() {
                return None;
            }
            let t0_cs = (e.t0.max(0.0) * 100.0).round() as i64;
            let t1_cs = (e.t1.max(0.0) * 100.0).round() as i64;
            Some(TimedWord {
                w,
                t0_cs,
                t1_cs: t1_cs.max(t0_cs),
            })
        })
        .collect();
    (!words.is_empty()).then_some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_write_load() {
        let dir = std::env::temp_dir().join(format!("tl_words_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("סרט.srt");
        let words = vec![
            TimedWord { w: "שלום".into(), t0_cs: 42, t1_cs: 81 },
            TimedWord { w: "עולם".into(), t0_cs: 90, t1_cs: 130 },
        ];
        write(&sidecar_path(&srt), "groq", &words).unwrap();
        assert!(dir.join("סרט.words.json").exists());
        let loaded = load_for_srt(&srt).expect("load");
        assert_eq!(loaded, words);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_bad_inputs() {
        let dir = std::env::temp_dir().join(format!("tl_words_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("x.srt");
        // Missing file
        assert!(load_for_srt(&srt).is_none());
        // Malformed JSON
        std::fs::write(dir.join("x.words.json"), "not json").unwrap();
        assert!(load_for_srt(&srt).is_none());
        // Wrong version
        std::fs::write(
            dir.join("x.words.json"),
            r#"{"version":99,"engine":"groq","language":"he","words":[{"w":"a","t0":0,"t1":1}]}"#,
        )
        .unwrap();
        assert!(load_for_srt(&srt).is_none());
        // Empty words
        std::fs::write(
            dir.join("x.words.json"),
            r#"{"version":1,"engine":"groq","language":"he","words":[]}"#,
        )
        .unwrap();
        assert!(load_for_srt(&srt).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_clamps_and_filters_entries() {
        let dir = std::env::temp_dir().join(format!("tl_words_clamp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("y.words.json"),
            // backwards times clamp; blank word dropped; NaN dropped
            r#"{"version":1,"engine":"whisper-local","language":"he","words":[
                {"w":"תקין","t0":1.0,"t1":0.5},
                {"w":"  ","t0":2.0,"t1":3.0},
                {"w":"גם","t0":4.0,"t1":4.5}
            ]}"#,
        )
        .unwrap();
        let words = load_for_srt(&dir.join("y.srt")).expect("load");
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].t0_cs, 100);
        assert_eq!(words[0].t1_cs, 100, "backwards t1 clamps to t0");
        assert_eq!(words[1].w, "גם");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
