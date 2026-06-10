//! SRT → ASS conversion: the styles/effects engine of the burn-in feature.
//!
//! Every preset goes through ASS — including `classic` — because correct Hebrew
//! RTL punctuation requires the libass-specific `Encoding: -1` style extension
//! (base-direction autodetection). Verified empirically (Phase 0, 2026-06-10):
//! plain SRT via force_style, RLM-injected SRT, and ASS with the default
//! Encoding all render sentence-final `.`/`?` on the wrong (right) side;
//! `Encoding: -1` fixes clean Hebrew, mixed Hebrew+Latin, numbers/time, and
//! multi-line cues. Do not "simplify" classic back to a force_style path.
//!
//! ASS is plain text — presets are string templates, no external crate.

use super::srt_parse::SrtCue;
use crate::video_transcription::words::TimedWord;

/// Burn style preset, deserialized from `settings.burn_style`. Unknown values
/// fall back to `Classic` so a stale/hand-edited settings.json never breaks a
/// burn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Classic,
    Box,
    Fade,
    Pop,
    /// Word-by-word highlight (CapCut-style). Requires the `words.json` sidecar;
    /// without it the burn degrades to `Box`.
    Karaoke,
    /// Neon glow: cyan-tinted blur halo around white text.
    Glow,
    /// Social-media impact: bold yellow with a heavy black outline.
    Impact,
    /// Each cue slides up into place with a soft fade.
    Rise,
    /// Minimal: no outline, just a large soft drop shadow.
    Shadow,
}

impl Preset {
    pub fn from_id(id: &str) -> Self {
        match id {
            "box" => Self::Box,
            "fade" => Self::Fade,
            "pop" => Self::Pop,
            "karaoke" => Self::Karaoke,
            "glow" => Self::Glow,
            "impact" => Self::Impact,
            "rise" => Self::Rise,
            "shadow" => Self::Shadow,
            _ => Self::Classic,
        }
    }
}

/// Fixed coordinate space; libass scales everything to the actual video size by
/// the PlayRes ratio, so one set of font/margin values fits all resolutions.
const PLAY_RES_X: u32 = 1280;
const PLAY_RES_Y: u32 = 720;

/// Builds a complete ASS document for `cues` in the given preset. Pure string
/// work — infallible; empty cues yield a header-only script.
pub fn build(cues: &[SrtCue], preset: Preset) -> String {
    let mut out = String::with_capacity(1024 + cues.len() * 96);
    out.push_str(&header(preset));
    for cue in cues {
        let text = cue
            .lines
            .iter()
            .map(|l| escape_ass(l))
            .collect::<Vec<_>>()
            .join("\\N");
        out.push_str(&format!(
            "Dialogue: 0,{},{},TL,,0,0,0,,{}{}\n",
            ass_ts(cue.start_cs),
            ass_ts(cue.end_cs),
            effect_prefix(preset),
            text
        ));
    }
    out
}

/// Script header + the single `TL` style. Per-preset differences live in the
/// style line (box/karaoke) or in per-event override tags (fade/pop).
///
/// Style anatomy (values calibrated visually in Phase 0 at 720p):
/// - `Arial` — present on every Windows install, full Hebrew glyph coverage,
///   resolved via DirectWrite; no bundled font needed.
/// - `BorderStyle` 1 = outline+shadow, 3 = opaque box (`box`/`karaoke`).
/// - `&H80000000` BackColour = half-transparent black (box fill).
/// - Karaoke colors: `\k` text starts in SecondaryColour (white) and flips to
///   PrimaryColour (yellow, `&H0000FFFF` = BGR) as each word is "sung" —
///   verified to advance right-to-left in Hebrew (Phase 0).
/// - **`Encoding: -1`** — the Hebrew RTL fix; see module docs.
fn header(preset: Preset) -> String {
    // (bold, border_style, outline, shadow, primary, outline_colour, back)
    // ASS colors are &HAABBGGRR. BackColour doubles as the box fill
    // (BorderStyle 3) or the shadow color (BorderStyle 1).
    let (bold, border_style, outline, shadow, primary, oc, back) = match preset {
        Preset::Karaoke => (0, 3, 2, 0, "&H0000FFFF", "&H00000000", "&H80000000"),
        Preset::Box => (0, 3, 2, 0, "&H00FFFFFF", "&H00000000", "&H80000000"),
        // Cyan halo; the per-event `\blur` melts the wide outline into a glow.
        Preset::Glow => (0, 1, 4, 0, "&H00FFFFFF", "&H00FFFF00", "&H00000000"),
        // Bold yellow + heavy black outline + a touch of shadow (social style).
        Preset::Impact => (-1, 1, 4, 1, "&H0000FFFF", "&H00000000", "&H80000000"),
        // No outline at all — a large, soft, semi-transparent drop shadow.
        Preset::Shadow => (0, 1, 0, 3, "&H00FFFFFF", "&H00000000", "&H80000000"),
        _ => (0, 1, 3, 0, "&H00FFFFFF", "&H00000000", "&H00000000"),
    };
    format!(
        "[Script Info]\n\
         ScriptType: v4.00+\n\
         PlayResX: {PLAY_RES_X}\n\
         PlayResY: {PLAY_RES_Y}\n\
         WrapStyle: 0\n\
         ScaledBorderAndShadow: yes\n\
         \n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: TL,Arial,48,{primary},&H00FFFFFF,{oc},{back},{bold},0,0,0,100,100,0,0,{border_style},{outline},{shadow},2,30,30,40,-1\n\
         \n\
         [Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
    )
}

/// Builds the karaoke (word-highlight) ASS document. For each cue it takes the
/// timed words whose midpoint falls inside the cue's window and verifies they
/// spell the cue's text (normalized — punctuation/marks stripped, so the
/// punctuated SRT still matches the raw engine words). A matching cue gets
/// `{\k<cs>}` per word; a cue that doesn't match (hand-edited SRT, drift)
/// falls back to plain rendering **for that cue only** — editing must never
/// break the burn. Returns the doc + how many cues actually got karaoke.
pub fn build_karaoke(cues: &[SrtCue], words: &[TimedWord]) -> (String, usize) {
    let mut out = String::with_capacity(1024 + cues.len() * 128);
    out.push_str(&header(Preset::Karaoke));
    let mut matched = 0usize;
    for cue in cues {
        let text = match karaoke_cue_text(cue, words) {
            Some(t) => {
                matched += 1;
                t
            }
            // Plain fallback: force white — without the override the cue would
            // inherit the style's yellow PrimaryColour and read as "highlighted"
            // (caught visually in the M2 verification burn).
            None => format!(
                "{{\\1c&HFFFFFF&}}{}",
                cue.lines
                    .iter()
                    .map(|l| escape_ass(l))
                    .collect::<Vec<_>>()
                    .join("\\N")
            ),
        };
        out.push_str(&format!(
            "Dialogue: 0,{},{},TL,,0,0,0,,{}\n",
            ass_ts(cue.start_cs),
            ass_ts(cue.end_cs),
            text
        ));
    }
    (out, matched)
}

/// Attempts the `\k`-tagged event text for one cue; `None` ⇒ render plain.
fn karaoke_cue_text(cue: &SrtCue, words: &[TimedWord]) -> Option<String> {
    // Words belonging to this cue: midpoint inside [start, end). Each word lands
    // in exactly one cue since cues don't overlap.
    let window: Vec<&TimedWord> = words
        .iter()
        .filter(|w| {
            let mid = (w.t0_cs + w.t1_cs) / 2;
            mid >= cue.start_cs && mid < cue.end_cs
        })
        .collect();
    if window.is_empty() {
        return None;
    }

    // The words must spell the cue text (normalized) — otherwise timings don't
    // describe what's on screen and highlighting would lie.
    let cue_tokens: Vec<&str> = cue.lines.iter().flat_map(|l| l.split_whitespace()).collect();
    if cue_tokens.len() != window.len() {
        return None;
    }
    for (tok, w) in cue_tokens.iter().zip(&window) {
        if normalize(tok) != normalize(&w.w) || normalize(tok).is_empty() {
            return None;
        }
    }

    // Assemble: per word a lead-gap empty syllable (silence between words stays
    // un-highlighted) + the word's own duration. Running total is clamped to the
    // cue duration so every highlight completes within the event.
    let cue_dur = (cue.end_cs - cue.start_cs).max(0);
    let mut text = String::new();
    let mut elapsed: i64 = 0; // cs since cue start already covered by \k tags
    let mut wi = 0usize;
    for (li, line) in cue.lines.iter().enumerate() {
        if li > 0 {
            text.push_str("\\N");
        }
        let count = line.split_whitespace().count();
        for (j, tok) in line.split_whitespace().enumerate() {
            let w = window[wi];
            let gap = (w.t0_cs - cue.start_cs - elapsed).clamp(0, cue_dur - elapsed);
            if gap > 0 {
                text.push_str(&format!("{{\\k{gap}}}"));
                elapsed += gap;
            }
            let dur = (w.t1_cs - w.t0_cs).clamp(0, cue_dur - elapsed);
            text.push_str(&format!("{{\\k{dur}}}{}", escape_ass(tok)));
            elapsed += dur;
            if j + 1 < count {
                text.push(' ');
            }
            wi += 1;
        }
    }
    Some(text)
}

/// Comparison form for matching SRT tokens to engine words: keep only letters
/// and digits (drops punctuation the auto-punctuation layer added, directional
/// marks, quotes), lowercase Latin. Hebrew letters pass through unchanged.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Per-event animation tags. Values are time-based (ms), so they look identical
/// at any frame rate (Phase 0 calibration: subtle, not cartoonish).
fn effect_prefix(preset: Preset) -> &'static str {
    match preset {
        Preset::Fade => "{\\fad(150,150)}",
        Preset::Pop => "{\\t(0,120,\\fscx108\\fscy108)\\t(120,220,\\fscx100\\fscy100)}",
        // Melt the wide outline into a halo.
        Preset::Glow => "{\\blur5}",
        // Slide up 32 px into the default \an2 anchor (x=640, y=720-MarginV).
        Preset::Rise => "{\\move(640,712,640,680,0,180)\\fad(120,120)}",
        // Soften the shadow edge.
        Preset::Shadow => "{\\blur1}",
        _ => "",
    }
}

/// Centiseconds → ASS timestamp `H:MM:SS.cc`. Negatives clamp to 0.
fn ass_ts(cs: i64) -> String {
    let cs = cs.max(0);
    let h = cs / 360_000;
    let m = (cs % 360_000) / 6_000;
    let s = (cs % 6_000) / 100;
    let c = cs % 100;
    format!("{h}:{m:02}:{s:02}.{c:02}")
}

/// Escapes characters libass would otherwise interpret: `{`/`}` open/close
/// override blocks. Subtitle text never legitimately contains override tags, so
/// braces are always literal here. (Stray backslashes are left as-is: `\N`-like
/// sequences are vanishingly rare in real subtitles, and escaping `\` itself is
/// not reliably supported across libass versions.)
fn escape_ass(line: &str) -> String {
    line.replace('{', "\\{").replace('}', "\\}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start_cs: i64, end_cs: i64, lines: &[&str]) -> SrtCue {
        SrtCue {
            start_cs,
            end_cs,
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn timestamps_format_as_ass() {
        assert_eq!(ass_ts(0), "0:00:00.00");
        assert_eq!(ass_ts(625), "0:00:06.25");
        assert_eq!(ass_ts(360_000 + 61 * 100 + 7), "1:01:01.07");
        assert_eq!(ass_ts(-5), "0:00:00.00");
    }

    const ALL_PRESETS: [Preset; 9] = [
        Preset::Classic,
        Preset::Box,
        Preset::Fade,
        Preset::Pop,
        Preset::Karaoke,
        Preset::Glow,
        Preset::Impact,
        Preset::Rise,
        Preset::Shadow,
    ];

    #[test]
    fn every_preset_has_rtl_encoding_and_core_fields() {
        for p in ALL_PRESETS {
            let ass = build(&[cue(0, 400, &["שלום."])], p);
            let style = ass.lines().find(|l| l.starts_with("Style: TL,")).unwrap();
            assert!(style.ends_with(",-1"), "Encoding must be -1 (RTL fix): {style}");
            assert!(style.contains("Arial,48"), "font/size: {style}");
            assert!(ass.contains("PlayResX: 1280"));
            assert!(ass.contains("Dialogue: 0,0:00:00.00,0:00:04.00,TL,,0,0,0,"));
        }
    }

    #[test]
    fn new_presets_emit_their_signature_fields() {
        let cues = [cue(0, 400, &["שלום"])];
        // Glow: cyan outline colour + blur prefix.
        let glow = build(&cues, Preset::Glow);
        assert!(glow.contains(",&H00FFFF00,"), "cyan outline:\n{glow}");
        assert!(glow.contains("{\\blur5}שלום"));
        // Impact: bold (-1), yellow primary, outline 4, shadow 1.
        let impact = build(&cues, Preset::Impact);
        let style = impact.lines().find(|l| l.starts_with("Style: TL,")).unwrap();
        assert!(style.contains(",&H0000FFFF,"), "yellow primary: {style}");
        assert_eq!(style.split(',').nth(7).unwrap(), "-1", "bold: {style}");
        assert_eq!(style.split(',').nth(16).unwrap(), "4", "outline: {style}");
        assert_eq!(style.split(',').nth(17).unwrap(), "1", "shadow: {style}");
        // Rise: slide-up move + fade prefix.
        let rise = build(&cues, Preset::Rise);
        assert!(rise.contains("{\\move(640,712,640,680,0,180)\\fad(120,120)}שלום"));
        // Shadow: zero outline, shadow 3, blur prefix.
        let shadow = build(&cues, Preset::Shadow);
        let style = shadow.lines().find(|l| l.starts_with("Style: TL,")).unwrap();
        assert_eq!(style.split(',').nth(16).unwrap(), "0", "no outline: {style}");
        assert_eq!(style.split(',').nth(17).unwrap(), "3", "soft shadow: {style}");
        assert!(shadow.contains("{\\blur1}שלום"));
    }

    #[test]
    fn box_uses_opaque_box_border_style() {
        let ass = build(&[cue(0, 100, &["א"])], Preset::Box);
        let style = ass.lines().find(|l| l.starts_with("Style: TL,")).unwrap();
        assert!(style.contains(",&H80000000,"), "semi-transparent back: {style}");
        // BorderStyle is the 16th comma-separated value in the style line.
        assert_eq!(style.split(',').nth(15).unwrap(), "3");
    }

    #[test]
    fn fade_and_pop_emit_their_tags_classic_does_not() {
        let cues = [cue(0, 100, &["א"])];
        assert!(build(&cues, Preset::Fade).contains("{\\fad(150,150)}א"));
        assert!(build(&cues, Preset::Pop).contains("\\fscx108"));
        let classic = build(&cues, Preset::Classic);
        assert!(classic.contains(",,א"), "no tag prefix expected");
        assert!(!classic.contains("\\fad") && !classic.contains("\\t("));
    }

    #[test]
    fn multiline_joins_with_ass_linebreak_and_braces_escape() {
        let ass = build(&[cue(0, 100, &["שורה {אחת}", "שורה שתיים"])], Preset::Classic);
        assert!(ass.contains("שורה \\{אחת\\}\\Nשורה שתיים"));
    }

    #[test]
    fn unknown_preset_id_falls_back_to_classic() {
        assert_eq!(Preset::from_id("classic"), Preset::Classic);
        assert_eq!(Preset::from_id("box"), Preset::Box);
        assert_eq!(Preset::from_id("glow"), Preset::Glow);
        assert_eq!(Preset::from_id("impact"), Preset::Impact);
        assert_eq!(Preset::from_id("rise"), Preset::Rise);
        assert_eq!(Preset::from_id("shadow"), Preset::Shadow);
        assert_eq!(Preset::from_id(""), Preset::Classic);
        assert_eq!(Preset::from_id("garbage"), Preset::Classic);
    }

    // ── karaoke ──────────────────────────────────────────────────────────────

    fn tw(w: &str, t0_cs: i64, t1_cs: i64) -> TimedWord {
        TimedWord { w: w.into(), t0_cs, t1_cs }
    }

    #[test]
    fn karaoke_emits_k_tags_with_gaps_and_durations() {
        // Cue 0–4 s; words at 0.5–1.0, 1.2–2.0 (gap 0.5 lead, 0.2 between).
        let cues = [cue(0, 400, &["שלום עולם"])];
        let words = [tw("שלום", 50, 100), tw("עולם", 120, 200)];
        let (ass, matched) = build_karaoke(&cues, &words);
        assert_eq!(matched, 1);
        assert!(
            ass.contains("{\\k50}{\\k50}שלום {\\k20}{\\k80}עולם"),
            "unexpected karaoke text:\n{ass}"
        );
        // Karaoke style: yellow primary, white secondary, opaque box.
        let style = ass.lines().find(|l| l.starts_with("Style: TL,")).unwrap();
        assert!(style.contains("&H0000FFFF,&H00FFFFFF"), "colors: {style}");
        assert_eq!(style.split(',').nth(15).unwrap(), "3", "BorderStyle: {style}");
        assert!(style.ends_with(",-1"), "Encoding -1 must hold for karaoke too");
    }

    #[test]
    fn karaoke_matches_despite_punctuation_and_case() {
        // SRT text was auto-punctuated; engine words are raw. Latin case differs.
        let cues = [cue(0, 300, &["פתחתי את Chrome, נכון?"])];
        let words = [
            tw("פתחתי", 0, 50),
            tw("את", 60, 80),
            tw("chrome", 90, 150),
            tw("נכון", 160, 220),
        ];
        let (ass, matched) = build_karaoke(&cues, &words);
        assert_eq!(matched, 1);
        // The on-screen text keeps the SRT's punctuation, not the raw word.
        assert!(ass.contains("Chrome, "), "SRT text must be preserved:\n{ass}");
        assert!(ass.contains("נכון?"), "question mark preserved:\n{ass}");
    }

    #[test]
    fn karaoke_mismatched_cue_falls_back_to_plain_others_still_match() {
        let cues = [
            cue(0, 200, &["טקסט שנערך ידנית לגמרי"]),
            cue(200, 400, &["שלום עולם"]),
        ];
        let words = [
            tw("משהו", 10, 60),
            tw("אחר", 70, 120),
            tw("שלום", 210, 260),
            tw("עולם", 270, 330),
        ];
        let (ass, matched) = build_karaoke(&cues, &words);
        assert_eq!(matched, 1, "only the second cue matches");
        // First cue rendered plain (no \k) and forced white, second with tags.
        assert!(
            ass.contains(",,{\\1c&HFFFFFF&}טקסט שנערך ידנית לגמרי\n"),
            "plain fallback must override the karaoke yellow:\n{ass}"
        );
        assert!(ass.contains("{\\k10}{\\k50}שלום"), "matched cue tagged:\n{ass}");
    }

    #[test]
    fn karaoke_two_line_cue_keeps_linebreak() {
        let cues = [cue(0, 400, &["שלום עולם", "מה נשמע"])];
        let words = [
            tw("שלום", 0, 50),
            tw("עולם", 50, 100),
            tw("מה", 150, 200),
            tw("נשמע", 200, 280),
        ];
        let (ass, matched) = build_karaoke(&cues, &words);
        assert_eq!(matched, 1);
        assert!(
            ass.contains("עולם\\N{\\k50}{\\k50}מה"),
            "\\N between lines, gap syllable after it:\n{ass}"
        );
    }

    #[test]
    fn karaoke_durations_clamp_to_cue_end() {
        // Word overruns the cue end (1.0s word in a 0.8s cue) → clamped.
        let cues = [cue(0, 80, &["ארוך"])];
        let words = [tw("ארוך", 0, 100)];
        let (ass, matched) = build_karaoke(&cues, &words);
        assert_eq!(matched, 1);
        assert!(ass.contains("{\\k80}ארוך"), "duration clamped to 80 cs:\n{ass}");
    }

    #[test]
    fn karaoke_empty_window_or_count_mismatch_is_plain() {
        let cues = [cue(0, 200, &["שלום עולם"])];
        // No words in window
        let (_, matched) = build_karaoke(&cues, &[tw("שלום", 500, 600)]);
        assert_eq!(matched, 0);
        // Word-count mismatch
        let (_, matched) = build_karaoke(&cues, &[tw("שלום", 0, 50)]);
        assert_eq!(matched, 0);
    }

    /// Writes one `build`-generated ASS per preset to `%TEMP%\timluli_preset_<id>.ass`
    /// for visual calibration with ffmpeg. `#[ignore]`d fixture generator:
    ///   cargo test --manifest-path src-tauri/Cargo.toml gen_preset_fixtures -- --ignored
    #[test]
    #[ignore]
    fn gen_preset_fixtures() {
        let cues = [
            cue(0, 300, &["שלום לכם, ברוכים הבאים."]),
            cue(350, 650, &["מה השעה עכשיו?"]),
        ];
        for (id, p) in [
            ("classic", Preset::Classic),
            ("box", Preset::Box),
            ("fade", Preset::Fade),
            ("pop", Preset::Pop),
            ("glow", Preset::Glow),
            ("impact", Preset::Impact),
            ("rise", Preset::Rise),
            ("shadow", Preset::Shadow),
        ] {
            let out = std::env::temp_dir().join(format!("timluli_preset_{id}.ass"));
            std::fs::write(&out, build(&cues, p)).expect("write fixture");
            println!("wrote {}", out.display());
        }
    }

    /// Writes a `build_karaoke`-generated ASS to `%TEMP%\timluli_kara_fixture.ass`
    /// for visual verification with ffmpeg (burn + inspect frames). `#[ignore]`d —
    /// it's a fixture generator, not an assertion:
    ///   cargo test --manifest-path src-tauri/Cargo.toml gen_karaoke_fixture -- --ignored
    #[test]
    #[ignore]
    fn gen_karaoke_fixture() {
        let cues = [
            cue(0, 500, &["אני מדבר עברית עכשיו ברצף"]),
            cue(600, 1100, &["אני פותח את Chrome, נכון?"]),
            // Hand-edited cue — must render plain (no karaoke) but still burn.
            cue(1200, 1500, &["שורה שנערכה ידנית"]),
        ];
        let words = [
            tw("אני", 0, 80),
            tw("מדבר", 80, 160),
            tw("עברית", 160, 240),
            tw("עכשיו", 240, 320),
            tw("ברצף", 320, 420),
            tw("אני", 600, 680),
            tw("פותח", 680, 760),
            tw("את", 760, 840),
            tw("chrome", 840, 920),
            tw("נכון", 920, 1020),
            tw("אחר", 1250, 1350),
            tw("לגמרי", 1350, 1450),
        ];
        let (ass, matched) = build_karaoke(&cues, &words);
        assert_eq!(matched, 2, "first two cues match, edited one doesn't");
        let out = std::env::temp_dir().join("timluli_kara_fixture.ass");
        std::fs::write(&out, &ass).expect("write fixture");
        println!("wrote {}", out.display());
    }
}
