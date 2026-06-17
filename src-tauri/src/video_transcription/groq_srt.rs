//! Groq cloud speech-to-text for the video→SRT path — the timestamped sibling of
//! [`crate::transcription::groq`] (which requests plain `text` for the audio→`.txt`
//! path and is left untouched).
//!
//! Differences from the audio path, per the spec's decisions:
//!   - `response_format=verbose_json` → per-segment `start`/`end`/`text`.
//!   - model `whisper-large-v3` (not `-turbo`) — the accuracy/cost trade chosen for
//!     Hebrew subtitles.
//!
//! Reuses the shared Groq key (DPAPI-encrypted in `secrets.json`) and returns the
//! same [`Segment`] currency the local engine produces, so both feed `srt::build_srt`.

use std::path::Path;

use serde::Deserialize;
use tauri::AppHandle;

use super::words::TimedWord;
use crate::whisper_local::inference::Segment;

const GROQ_STT_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
/// Distinct from the audio→txt path's `whisper-large-v3-turbo`: subtitles favor
/// accuracy over throughput for Hebrew.
const GROQ_SUBTITLE_MODEL: &str = "whisper-large-v3";
/// Segments whose no-speech probability is at/above this are dropped as likely
/// silence/noise hallucinations — a cheap cloud-side guard (no VAD needed in v1).
/// Conservative on purpose: real speech sits far below this.
const NO_SPEECH_DROP: f32 = 0.85;
/// Max 429/5xx retries per slice before giving up (then the scheduler drops just
/// that slice). 5 with the cap below tolerates a long free-tier cooldown.
const MAX_STT_RETRIES: u32 = 5;
/// Upper bound on a single retry wait (s). Groq's audio-seconds reset can be ~33 min
/// on the free tier; we never block one slice that long — we retry sooner and let
/// the budget keep refilling between attempts.
const RETRY_CAP_SECS: f64 = 180.0;

/// Uploads `input` (a 16 kHz mono FLAC the caller extracted via ffmpeg) and returns
/// timed segments plus word-level timestamps (for the karaoke burn style's
/// `words.json` sidecar — may be empty if the API omits them). Hebrew error
/// strings, matching the rest of the app.
pub async fn transcribe_to_segments(
    app: &AppHandle,
    input: &Path,
    lang: &str,
) -> Result<(Vec<Segment>, Vec<TimedWord>), String> {
    let key = crate::secrets::get_key(app, "groq").ok_or_else(|| {
        "לא הוגדר מפתח Groq. חבר שירות תרגום בהגדרות → תרגום מסמכים כדי לתמלל בענן.".to_string()
    })?;

    let bytes = tokio::fs::read(input)
        .await
        .map_err(|e| format!("שגיאה בקריאת הקובץ: {e}"))?;
    let filename = input
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("audio.flac")
        .to_string();

    let client = reqwest::Client::new();

    // Rate-aware retry: on 429 (or a transient 5xx / network blip) wait exactly what
    // Groq's `Retry-After` / `x-ratelimit-reset-*` headers say, then retry the same
    // slice — never drop it. This self-paces against whatever tier the key is on
    // (free 7200 audio-s/h vs paid 200000) so the parallel scheduler can push to the
    // limit and let the server throttle, instead of silently losing slices.
    let mut attempt = 0u32;
    loop {
        // The multipart Form is consumed by `send`, so rebuild it each attempt.
        let file_part = reqwest::multipart::Part::bytes(bytes.clone())
            .file_name(filename.clone())
            .mime_str("application/octet-stream")
            .map_err(|e| format!("שגיאה בהכנת הקובץ: {e}"))?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", GROQ_SUBTITLE_MODEL)
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word")
            .text("timestamp_granularities[]", "segment");
        // `lang` empty/"auto" ⇒ omit so Whisper auto-detects the source language; a
        // specific ISO-639-1 code (e.g. "he") pins it.
        if !lang.is_empty() && !lang.eq_ignore_ascii_case("auto") {
            form = form.text("language", lang.to_string());
        }

        let resp = match client.post(GROQ_STT_URL).bearer_auth(&key).multipart(form).send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < MAX_STT_RETRIES {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
                return Err(format!("שגיאת רשת בתמלול בענן: {e}"));
            }
        };

        let status = resp.status();
        if status.is_success() {
            let body = resp
                .text()
                .await
                .map_err(|e| format!("שגיאה בקריאת התשובה: {e}"))?;
            return parse_response(&body);
        }

        // 429 / transient 5xx → honor the server's stated wait and retry the slice.
        if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_STT_RETRIES {
            let wait = retry_wait_secs(resp.headers());
            attempt += 1;
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait)).await;
            continue;
        }

        let body = resp.text().await.unwrap_or_default();
        return Err(match status.as_u16() {
            401 | 403 => "מפתח Groq לא תקין. בדוק את החיבור לשירות בהגדרות.".into(),
            413 => "קובץ הווידאו גדול מדי לתמלול בענן. נסה מנוע מקומי או קובץ קצר יותר.".into(),
            429 => "חרגת ממכסת השימוש ב-Groq. נסה שוב מאוחר יותר או השתמש במנוע מקומי.".into(),
            _ => format!("התמלול בענן נכשל ({status}): {}", body.trim()),
        });
    }
}

/// Seconds to wait before retrying a 429/5xx, read from Groq's headers: prefer
/// `Retry-After` (plain seconds), then the audio-seconds / requests reset windows
/// (Groq's `"1m26.4s"` / `"43.2s"` / `"432ms"` duration format). Clamped to a sane
/// band so one slice can't stall the job for a full 33-minute free-tier window.
fn retry_wait_secs(h: &reqwest::header::HeaderMap) -> f64 {
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    if let Some(ra) = get("retry-after") {
        if let Ok(s) = ra.trim().parse::<f64>() {
            return s.clamp(1.0, RETRY_CAP_SECS);
        }
        if let Some(d) = parse_groq_duration(&ra) {
            return d.clamp(1.0, RETRY_CAP_SECS);
        }
    }
    for n in ["x-ratelimit-reset-audio-seconds", "x-ratelimit-reset-requests"] {
        if let Some(v) = get(n) {
            if let Some(d) = parse_groq_duration(&v) {
                return d.clamp(1.0, RETRY_CAP_SECS);
            }
        }
    }
    5.0
}

/// Parses Groq's rate-limit duration strings (`"432ms"`, `"5.4s"`, `"1m26.4s"`,
/// `"32m29.5s"`) into seconds.
fn parse_groq_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(ms) = s.strip_suffix("ms") {
        return ms.trim().parse::<f64>().ok().map(|v| v / 1000.0);
    }
    let mut total = 0.0;
    let mut rest = s;
    if let Some(idx) = rest.find('m') {
        total += rest[..idx].parse::<f64>().ok()? * 60.0;
        rest = &rest[idx + 1..];
    }
    let rest = rest.strip_suffix('s').unwrap_or(rest);
    if !rest.is_empty() {
        total += rest.parse::<f64>().ok()?;
    }
    Some(total)
}

/// Shape of the relevant fields in Groq's `verbose_json` transcription response.
/// Unlisted fields (tokens, avg_logprob, …) are ignored.
#[derive(Deserialize)]
struct VerboseResponse {
    #[serde(default)]
    segments: Vec<VerboseSegment>,
    #[serde(default)]
    words: Vec<VerboseWord>,
}

#[derive(Deserialize)]
struct VerboseSegment {
    start: f64,
    end: f64,
    text: String,
    #[serde(default)]
    no_speech_prob: f32,
}

#[derive(Deserialize)]
struct VerboseWord {
    word: String,
    start: f64,
    end: f64,
}

/// Maps a `verbose_json` body to [`Segment`]s + [`TimedWord`]s (seconds →
/// centiseconds), dropping empty and high-no-speech-probability segments. Words
/// are best-effort: an absent/empty array is fine (older responses, no speech).
fn parse_response(body: &str) -> Result<(Vec<Segment>, Vec<TimedWord>), String> {
    let parsed: VerboseResponse = serde_json::from_str(body)
        .map_err(|e| format!("שגיאה בפענוח תשובת התמלול: {e}"))?;

    let segments = parsed
        .segments
        .into_iter()
        .filter(|s| s.no_speech_prob < NO_SPEECH_DROP)
        .filter_map(|s| {
            let text = s.text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let start_cs = (s.start.max(0.0) * 100.0).round() as i64;
            let end_cs = (s.end.max(0.0) * 100.0).round() as i64;
            Some(Segment {
                start_cs,
                end_cs: end_cs.max(start_cs),
                text,
            })
        })
        .collect();

    let words = parsed
        .words
        .into_iter()
        .filter_map(|w| {
            let text = w.word.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let t0_cs = (w.start.max(0.0) * 100.0).round() as i64;
            let t1_cs = (w.end.max(0.0) * 100.0).round() as i64;
            Some(TimedWord {
                w: text,
                t0_cs,
                t1_cs: t1_cs.max(t0_cs),
            })
        })
        .collect();

    Ok((segments, words))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed-down but realistic Groq verbose_json payload (extra fields included
    // to prove they're ignored; one silence/hallucination segment to prove it's
    // dropped).
    const SAMPLE: &str = r#"{
        "task": "transcribe",
        "language": "hebrew",
        "duration": 26.98,
        "text": "...",
        "segments": [
            {"id":0,"seek":0,"start":0.0,"end":10.22,"text":" דיברתי על מה ששלחת","tokens":[1,2],"avg_logprob":-0.2,"no_speech_prob":0.01},
            {"id":1,"seek":1022,"start":10.22,"end":18.94,"text":"ניסיתי גם לכתוב לאורן","no_speech_prob":0.03},
            {"id":2,"seek":1894,"start":18.94,"end":20.0,"text":"   ","no_speech_prob":0.04},
            {"id":3,"seek":2000,"start":20.0,"end":23.0,"text":"[music]","no_speech_prob":0.97}
        ]
    }"#;

    #[test]
    fn parses_groq_rate_limit_durations() {
        assert_eq!(super::parse_groq_duration("432ms"), Some(0.432));
        assert_eq!(super::parse_groq_duration("5.4s"), Some(5.4));
        assert_eq!(super::parse_groq_duration("43.2s"), Some(43.2));
        assert_eq!(super::parse_groq_duration("1m26.4s"), Some(86.4));
        assert_eq!(super::parse_groq_duration("32m29.5s"), Some(1949.5));
        assert_eq!(super::parse_groq_duration(""), None);
    }

    #[test]
    fn parses_segments_seconds_to_centiseconds() {
        let (segs, words) = parse_response(SAMPLE).expect("parse");
        // empty-text (#2) and high-no_speech (#3) segments dropped → 2 remain
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].start_cs, 0);
        assert_eq!(segs[0].end_cs, 1022);
        assert_eq!(segs[0].text, "דיברתי על מה ששלחת"); // leading space trimmed
        assert_eq!(segs[1].start_cs, 1022);
        assert_eq!(segs[1].end_cs, 1894);
        // no `words` array in this payload → empty, not an error
        assert!(words.is_empty());
    }

    #[test]
    fn parses_words_alongside_segments() {
        // Field shape captured live from the API in Phase 0 (word + start/end secs).
        let body = r#"{
            "task": "transcribe", "language": "Hebrew", "duration": 12.39,
            "text": " שלום עולם.",
            "words": [
                {"word":"שלום","start":0,"end":1.94},
                {"word":" עולם. ","start":2.1,"end":3.8},
                {"word":"   ","start":4.0,"end":4.2},
                {"word":"הפוך","start":6.0,"end":5.0}
            ],
            "segments": [
                {"id":0,"seek":0,"start":0,"end":3.8,"text":" שלום עולם.","no_speech_prob":0.03}
            ]
        }"#;
        let (segs, words) = parse_response(body).expect("parse");
        assert_eq!(segs.len(), 1);
        assert_eq!(words.len(), 3, "blank word dropped");
        assert_eq!(words[0].w, "שלום");
        assert_eq!(words[0].t0_cs, 0);
        assert_eq!(words[0].t1_cs, 194);
        assert_eq!(words[1].w, "עולם.", "word text trimmed");
        assert_eq!(words[2].t0_cs, 600);
        assert_eq!(words[2].t1_cs, 600, "backwards end clamps to start");
    }

    #[test]
    fn fractional_seconds_round_to_nearest_centisecond() {
        let body = r#"{"segments":[{"start":1.234,"end":2.999,"text":"x","no_speech_prob":0.0}]}"#;
        let (segs, _) = parse_response(body).expect("parse");
        assert_eq!(segs[0].start_cs, 123); // 1.234 s → 123.4 cs → 123
        assert_eq!(segs[0].end_cs, 300); // 2.999 s → 299.9 cs → 300
    }

    #[test]
    fn missing_segments_field_is_empty_not_error() {
        let (segs, words) = parse_response(r#"{"task":"transcribe","text":"hi"}"#).expect("parse");
        assert!(segs.is_empty());
        assert!(words.is_empty());
    }

    #[test]
    fn malformed_json_is_a_hebrew_error() {
        let err = parse_response("not json").unwrap_err();
        assert!(err.contains("פענוח"), "expected Hebrew parse error, got: {err}");
    }

    #[test]
    fn end_never_precedes_start() {
        // Defensive: a backwards segment clamps end up to start rather than emitting
        // a negative-duration cue.
        let body = r#"{"segments":[{"start":5.0,"end":4.0,"text":"x","no_speech_prob":0.0}]}"#;
        let (segs, _) = parse_response(body).expect("parse");
        assert_eq!(segs[0].start_cs, 500);
        assert_eq!(segs[0].end_cs, 500);
    }
}
