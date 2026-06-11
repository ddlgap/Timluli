//! SRT subtitle assembly from timed transcription segments (centisecond bounds).
//!
//! Pure, dependency-free formatting. Given `[Segment { start_cs, end_cs, text }]`
//! (from the local whisper engine, or later the Groq verbose_json path), it:
//!   1. drops consecutive duplicate segments — a common whisper long-form
//!      hallucination (the model "sticks" on a phrase and repeats it);
//!   2. splits over-long segments into player-friendly cues (≤2 lines,
//!      ~42 chars/line, ~6 s/cue), dividing time proportionally by character
//!      count (no word-level timestamps — WhisperX alignment is deliberately
//!      avoided, it does not work for Hebrew);
//!   3. word-wraps each cue into ≤2 lines;
//!   4. renders UTF-8, logical-order SRT (RTL "option A" — correct for modern
//!      players; an old-player punctuation-flip toggle is deferred to phase 2).
//!
//! Output uses `\n` line endings (accepted by VLC/mpv/web `<track>`); the caller
//! writes it BOM-less UTF-8 next to the source.

use crate::whisper_local::inference::Segment;

const MAX_LINE_CHARS: usize = 42;
const MAX_CUE_LINES: usize = 2;
/// Max characters per cue before it must be split (2 lines × line width).
const MAX_CUE_CHARS: usize = MAX_LINE_CHARS * MAX_CUE_LINES;
/// Soft cap on cue duration, in centiseconds (~6 s).
const MAX_CUE_CS: i64 = 600;

/// Builds an SRT document from timed segments. Infallible — empty input yields an
/// empty string (the caller decides whether that is an error). The production
/// pipeline calls `build_cues` + `render_srt` separately (it needs the cue list
/// for gender classification); this composition remains the test-facing entry.
#[cfg(test)]
pub fn build_srt(segments: &[Segment]) -> String {
    render_srt(&build_cues(segments))
}

/// The final cue list `(start_cs, end_cs, text)` exactly as `build_srt` numbers
/// it (cue N = index N-1). Exposed so the gender-classification pass can analyze
/// the audio window of each *rendered* cue — segment indices don't survive the
/// dedup/split below.
pub fn build_cues(segments: &[Segment]) -> Vec<(i64, i64, String)> {
    let deduped = dedup_consecutive(segments);

    let mut raw: Vec<(i64, i64, String)> = Vec::new();
    for seg in &deduped {
        raw.extend(split_segment(seg));
    }
    // Strip a stray leading mark some transcriptions emit at a cue start (e.g. ", "),
    // and drop any cue that becomes empty — before numbering, so indices stay gap-free.
    raw.into_iter()
        .filter_map(|(s, e, t)| {
            let cleaned = trim_cue_lead(&t).to_string();
            (!cleaned.is_empty()).then_some((s, e, cleaned))
        })
        .collect()
}

/// Renders numbered SRT text from a final cue list.
pub fn render_srt(cues: &[(i64, i64, String)]) -> String {
    let mut out = String::new();
    for (i, (start, end, text)) in cues.iter().enumerate() {
        out.push_str(&(i + 1).to_string());
        out.push('\n');
        out.push_str(&fmt_ts(*start));
        out.push_str(" --> ");
        out.push_str(&fmt_ts(*end));
        out.push('\n');
        out.push_str(&wrap_lines(text));
        out.push('\n');
        out.push('\n');
    }
    out
}

/// Formats centiseconds as an SRT timestamp `HH:MM:SS,mmm`. Negatives clamp to 0.
fn fmt_ts(cs: i64) -> String {
    let ms = cs.max(0) * 10;
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    let millis = ms % 1000;
    format!("{h:02}:{m:02}:{s:02},{millis:03}")
}

/// Strips leading whitespace and stray sentence punctuation a transcriber sometimes
/// emits at the very start of a cue (e.g. ", שלום"), plus trailing whitespace.
/// Interior marks and opening quotes/parens are left untouched.
fn trim_cue_lead(text: &str) -> &str {
    text.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '،' | '؛' | '؟')
    })
    .trim_end()
}

/// Merges consecutive segments with identical trimmed text (extending the first's
/// end over the duplicates) and drops empties. Targets whisper's repeated-phrase
/// hallucination, not the distinct sub-cues produced by splitting.
fn dedup_consecutive(segments: &[Segment]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for seg in segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last.text == text {
                last.end_cs = last.end_cs.max(seg.end_cs);
                continue;
            }
        }
        out.push(Segment {
            start_cs: seg.start_cs,
            end_cs: seg.end_cs.max(seg.start_cs),
            text: text.to_string(),
        });
    }
    out
}

/// Splits one segment into `n` cues so each respects the duration and character
/// caps, dividing the segment's time span proportionally to each part's character
/// count. Returns `(start_cs, end_cs, text)` tuples; contiguous and ordered, with
/// the final cue ending exactly at `seg.end_cs`.
fn split_segment(seg: &Segment) -> Vec<(i64, i64, String)> {
    let words: Vec<&str> = seg.text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let total_chars = seg.text.chars().count().max(1);
    let dur = (seg.end_cs - seg.start_cs).max(0);

    // `dur` is non-negative, so route through stable usize::div_ceil
    // (i64::div_ceil is still unstable on this toolchain).
    let by_dur = (dur as usize).div_ceil(MAX_CUE_CS as usize).max(1);
    let by_chars = total_chars.div_ceil(MAX_CUE_CHARS).max(1);
    let n = by_dur.max(by_chars);
    if n <= 1 {
        return vec![(seg.start_cs, seg.end_cs, seg.text.trim().to_string())];
    }

    // Greedily pack words into n parts of ~equal character length. The
    // `parts.len() < n - 1` guard keeps room so we never exceed n parts.
    let target = total_chars.div_ceil(n);
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for w in &words {
        let extra = usize::from(!cur.is_empty());
        if !cur.is_empty()
            && cur.chars().count() + extra + w.chars().count() > target
            && parts.len() < n - 1
        {
            parts.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        parts.push(cur);
    }

    // Even time slices (each = dur / parts.len()). Because parts.len() ≥
    // ceil(dur / MAX_CUE_CS), no cue can exceed the ~6 s cap — the guarantee a
    // char-proportional split can't make when speech is sparse. The text is already
    // split into ~equal-character parts above, so under a roughly constant speaking
    // rate the slices line up with the words. The last cue ends exactly at end_cs.
    let p = parts.len() as i64;
    let mut cues = Vec::with_capacity(parts.len());
    for (i, part) in parts.into_iter().enumerate() {
        let idx = i as i64;
        let start = seg.start_cs + dur * idx / p;
        let end = if idx + 1 == p {
            seg.end_cs
        } else {
            seg.start_cs + dur * (idx + 1) / p
        };
        cues.push((start, end, part));
    }
    cues
}

/// Word-wraps `text` into up to [`MAX_CUE_LINES`] lines of ≤ [`MAX_LINE_CHARS`]
/// characters (greedy). If it would exceed the line limit, the overflow is folded
/// into the last line (graceful degradation for the rare over-long word).
fn wrap_lines(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for w in text.split_whitespace() {
        let extra = usize::from(!cur.is_empty());
        if !cur.is_empty() && cur.chars().count() + extra + w.chars().count() > MAX_LINE_CHARS {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > MAX_CUE_LINES {
        let mut folded: Vec<String> = lines[..MAX_CUE_LINES - 1].to_vec();
        folded.push(lines[MAX_CUE_LINES - 1..].join(" "));
        lines = folded;
    }
    // Plain logical-order UTF-8 (Option A): a bidi-capable player (VLC/mpv) renders
    // Hebrew RTL with sentence-final punctuation on the left. No directional marks —
    // they're unnecessary here and VLC renders RLM/LRM as a stray glyph (vlc#13059).
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_cs: i64, end_cs: i64, text: &str) -> Segment {
        Segment {
            start_cs,
            end_cs,
            text: text.to_string(),
        }
    }

    // ── tiny SRT parser, for asserting on built output ────────────────────────
    struct Cue {
        start_ms: i64,
        end_ms: i64,
        line_count: usize,
        max_line_chars: usize,
    }
    fn ts_to_ms(ts: &str) -> i64 {
        let (hms, mmm) = ts.split_once(',').expect("comma in ts");
        let p: Vec<i64> = hms.split(':').map(|x| x.parse().unwrap()).collect();
        (p[0] * 3600 + p[1] * 60 + p[2]) * 1000 + mmm.parse::<i64>().unwrap()
    }
    fn parse_cues(srt: &str) -> Vec<Cue> {
        let mut cues = Vec::new();
        for block in srt.split("\n\n") {
            let lines: Vec<&str> = block.lines().collect();
            if lines.len() < 3 {
                continue;
            }
            let (a, b) = lines[1].split_once(" --> ").expect("arrow");
            let text_lines = &lines[2..];
            cues.push(Cue {
                start_ms: ts_to_ms(a),
                end_ms: ts_to_ms(b),
                line_count: text_lines.len(),
                max_line_chars: text_lines.iter().map(|l| l.chars().count()).max().unwrap_or(0),
            });
        }
        cues
    }

    #[test]
    fn timestamp_format() {
        assert_eq!(fmt_ts(0), "00:00:00,000");
        assert_eq!(fmt_ts(52), "00:00:00,520");
        assert_eq!(fmt_ts(2698), "00:00:26,980");
        assert_eq!(fmt_ts(360_000), "01:00:00,000");
        assert_eq!(fmt_ts(-5), "00:00:00,000");
    }

    #[test]
    fn short_segment_is_one_cue() {
        let srt = build_srt(&[seg(0, 52, "שלום.")]);
        assert!(
            srt.starts_with("1\n00:00:00,000 --> 00:00:00,520\nשלום.\n\n"),
            "unexpected SRT:\n{srt}"
        );
        assert_eq!(parse_cues(&srt).len(), 1);
    }

    #[test]
    fn empty_input_is_empty_string() {
        assert_eq!(build_srt(&[]), "");
        // whitespace-only / empty segments are dropped
        assert_eq!(build_srt(&[seg(0, 100, "   ")]), "");
    }

    #[test]
    fn long_segment_splits_ordered_contiguous_within_caps() {
        // The real captured segment: 10.22 s, ~99 chars → must split.
        let text = "דיברתי על מה ששלחת, קודם כל נראה וואו ברמות, הלוואי שנצליח להגיע למשהו דומה ברמת הביצוע, באמת הלוואי.";
        let srt = build_srt(&[seg(0, 1022, text)]);
        let cues = parse_cues(&srt);

        assert!(cues.len() >= 2, "expected a split, got {} cue(s)", cues.len());
        // ordered, contiguous, last ends exactly at 10.22 s
        let mut prev_end = 0;
        for c in &cues {
            assert!(c.start_ms >= prev_end, "cue starts before previous ended");
            assert!(c.end_ms >= c.start_ms, "cue ends before it starts");
            assert!(c.line_count <= MAX_CUE_LINES, "cue has {} lines", c.line_count);
            assert!(
                c.max_line_chars <= MAX_LINE_CHARS,
                "line too long: {} chars",
                c.max_line_chars
            );
            prev_end = c.end_ms;
        }
        assert_eq!(cues.last().unwrap().end_ms, 10_220, "last cue must end at segment end");
    }

    #[test]
    fn very_long_segment_respects_duration_cap() {
        // 30 s single segment, modest text → split driven by the ~6 s duration cap.
        let srt = build_srt(&[seg(0, 3000, "מילה אחת שתיים שלוש ארבע חמש שש שבע שמונה")]);
        let cues = parse_cues(&srt);
        assert!(cues.len() >= 5, "30 s should split into ≥5 cues, got {}", cues.len());
        for c in &cues {
            // Even slices: each ≤ ceil(dur/p) ≤ MAX_CUE_CS + 1 cs → +20 ms covers rounding.
            assert!(c.end_ms - c.start_ms <= MAX_CUE_CS * 10 + 20, "cue exceeds ~6 s");
        }
    }

    #[test]
    fn dedups_consecutive_repeats_and_extends_end() {
        let segs = [
            seg(0, 100, "חזרה"),
            seg(100, 200, "חזרה"),
            seg(200, 300, "שונה"),
        ];
        let cues = parse_cues(&build_srt(&segs));
        assert_eq!(cues.len(), 2, "consecutive duplicate not merged");
        assert_eq!(cues[0].end_ms, 2000, "first cue should extend over the duplicate");
        assert_eq!(cues[1].start_ms, 2000);
    }

    #[test]
    fn cue_numbering_is_sequential() {
        let text = "אחת שתיים שלוש ארבע חמש שש שבע שמונה תשע עשר אחת עשרה שתים עשרה שלוש עשרה ארבע עשרה חמש עשרה שש עשרה";
        let srt = build_srt(&[seg(0, 1800, text)]);
        let indices: Vec<&str> = srt
            .split("\n\n")
            .filter(|b| !b.trim().is_empty())
            .map(|b| b.lines().next().unwrap())
            .collect();
        for (i, idx) in indices.iter().enumerate() {
            assert_eq!(*idx, (i + 1).to_string(), "cue numbering not sequential");
        }
    }

    #[test]
    fn strips_leading_stray_punctuation() {
        let srt = build_srt(&[seg(0, 200, ", שלום לכם")]);
        assert!(srt.contains("שלום לכם"), "text missing:\n{srt}");
        assert!(!srt.contains(", שלום"), "leading comma survived:\n{srt}");
        // a cue that is *only* punctuation is dropped entirely
        assert_eq!(build_srt(&[seg(0, 100, " . , ")]), "");
    }

    #[test]
    fn output_is_plain_logical_order_no_directional_marks() {
        // Option A: no RLM/LRM/RLE/etc. — VLC/mpv bidi handles RTL; marks would only
        // risk VLC rendering a stray glyph (vlc#13059).
        let srt = build_srt(&[seg(0, 300, "שלום לכם, מה שלומכם?")]);
        for mark in ['\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{feff}'] {
            assert!(!srt.contains(mark), "unexpected directional mark U+{:04X}", mark as u32);
        }
    }
}
