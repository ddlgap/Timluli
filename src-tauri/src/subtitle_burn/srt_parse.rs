//! Lenient SRT parsing for the burn-in pipeline.
//!
//! Accepts the SRT files Timluli generates (`video_transcription::srt`) as well
//! as common "wild" variants: CRLF line endings, a UTF-8 BOM, `.` instead of `,`
//! as the millisecond separator, missing/garbled cue indices, and stray blank
//! lines. Malformed blocks are skipped rather than failing the whole file — a
//! single bad cue must never abort a burn (degradation, not failure).

/// One subtitle cue. `lines` preserves the SRT's own line breaks (rendered as
/// `\N` in ASS); times are centiseconds, matching the rest of the codebase.
#[derive(Debug, Clone, PartialEq)]
pub struct SrtCue {
    pub start_cs: i64,
    pub end_cs: i64,
    pub lines: Vec<String>,
}

/// Parses an SRT document into cues, ordered as they appear. Returns an empty
/// vec when nothing parseable is found (the caller decides whether to error).
pub fn parse(srt: &str) -> Vec<SrtCue> {
    let text = srt.trim_start_matches('\u{feff}');
    let mut cues = Vec::new();

    // Split into blocks on blank lines (any mix of \r\n / \n, repeated).
    for block in text.split("\n\n").flat_map(|b| b.split("\r\n\r\n")) {
        let lines: Vec<&str> = block.lines().map(str::trim_end).collect();
        // Find the timing line — usually line 1 (after the index), but be
        // tolerant of a missing index or leading junk.
        let Some(t_idx) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let Some((start_cs, end_cs)) = parse_timing(lines[t_idx]) else {
            continue;
        };
        let text_lines: Vec<String> = lines[t_idx + 1..]
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if text_lines.is_empty() {
            continue;
        }
        cues.push(SrtCue {
            start_cs,
            end_cs: end_cs.max(start_cs),
            lines: text_lines,
        });
    }
    cues
}

/// Parses `HH:MM:SS,mmm --> HH:MM:SS,mmm` (also `.` for `,`). Ignores trailing
/// cue settings (`X1:… line:…`) some tools append after the second timestamp.
fn parse_timing(line: &str) -> Option<(i64, i64)> {
    let (a, b) = line.split_once("-->")?;
    let start = parse_ts(a.trim())?;
    let end = parse_ts(b.split_whitespace().next()?)?;
    Some((start, end))
}

/// `HH:MM:SS,mmm` → centiseconds. Hours may exceed two digits.
fn parse_ts(ts: &str) -> Option<i64> {
    let (hms, ms) = ts
        .split_once(',')
        .or_else(|| ts.split_once('.'))
        .unwrap_or((ts, "0"));
    let mut parts = hms.split(':');
    let h: i64 = parts.next()?.trim().parse().ok()?;
    let m: i64 = parts.next()?.trim().parse().ok()?;
    let s: i64 = parts.next()?.trim().parse().ok()?;
    // Millis may be 1-3 digits; right-pad to 3 ("5" → 500 ms is wrong, but
    // "5" in the wild means ".5s" → 500 ms, so pad is correct).
    let ms: i64 = format!("{:0<3}", ms.trim().chars().take(3).collect::<String>())
        .parse()
        .ok()?;
    Some(((h * 3600 + m * 60 + s) * 1000 + ms) / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_srt() {
        let srt = "1\n00:00:00,000 --> 00:00:04,000\nשלום לכם.\n\n2\n00:00:04,000 --> 00:00:08,000\nמה השעה?\n";
        let cues = parse(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_cs, 0);
        assert_eq!(cues[0].end_cs, 400);
        assert_eq!(cues[0].lines, vec!["שלום לכם."]);
        assert_eq!(cues[1].start_cs, 400);
    }

    #[test]
    fn handles_crlf_bom_and_multiline() {
        let srt = "\u{feff}1\r\n00:00:01,500 --> 00:00:06,250\r\nשורה ראשונה,\r\nוגם שנייה\r\n\r\n";
        let cues = parse(srt);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_cs, 150);
        assert_eq!(cues[0].end_cs, 625);
        assert_eq!(cues[0].lines.len(), 2);
    }

    #[test]
    fn dot_separator_and_missing_index() {
        let srt = "00:00:00.500 --> 00:00:02.000\nטקסט\n";
        let cues = parse(srt);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_cs, 50);
    }

    #[test]
    fn skips_malformed_blocks_keeps_good_ones() {
        let srt = "1\nגיבריש בלי תזמון\n\n2\n00:00:02,000 --> 00:00:03,000\nתקין\n\n3\n00:00:09,000 --> 00:00:08,000\nהפוך\n";
        let cues = parse(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].lines, vec!["תקין"]);
        // Backwards timing clamps end to start instead of dropping the cue.
        assert_eq!(cues[1].start_cs, 900);
        assert_eq!(cues[1].end_cs, 900);
    }

    #[test]
    fn empty_and_garbage_inputs_yield_no_cues() {
        assert!(parse("").is_empty());
        assert!(parse("   \n\n  ").is_empty());
        assert!(parse("לא srt בכלל").is_empty());
    }

    #[test]
    fn hours_over_99_and_short_millis() {
        let cues = parse("1\n100:00:00,5 --> 100:00:01,25\nארוך\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_cs, 100 * 360_000 + 50);
        assert_eq!(cues[0].end_cs, 100 * 360_000 + 125);
    }
}
