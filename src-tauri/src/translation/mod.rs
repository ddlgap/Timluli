//! Document translation engine (Rust port of cerabras/translate_drop.py).
//! Parses a subtitle/text/office file into translatable units, translates them
//! batch-by-batch through a provider fallback chain, then writes a translated
//! copy next to the original. Provider HTTP goes out from Rust (reqwest), so it
//! is not subject to the webview CSP.

mod docx;
mod parser;
mod pdf;
mod provider;

use futures_util::stream::{self, StreamExt};
use parser::Category;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub use provider::ModelInfo;

/// Default fallback order, **quality-first**. Ordered by a live Hebrew-translation
/// benchmark of every Groq/Cerebras model (see the QA report): `gpt-oss-120b` is the
/// most accurate, `llama-4-scout` is nearly as good and the fastest, then
/// `llama-3.3-70b`. Groq leads because it is the reliably-available provider; the
/// Cerebras entries (same top model — faster when that key is active) follow as
/// backup and are skipped cheaply when the key is out of quota. `qwen3-32b`
/// (leaks chain-of-thought into the output) and `allam-2-7b` (garbled Hebrew) are
/// deliberately excluded. A failing model re-walks the chain (see `run_batch` +
/// `classify`).
///
/// Cerebras model ids must track its (now narrow) live catalogue: as of 2026-06-15
/// the API serves only `gpt-oss-120b` and `zai-glm-4.7` — every older llama/qwen id
/// (incl. the previously-listed `qwen-3-235b-a22b-instruct-2507`) returns 404
/// `model_not_found`.
const FALLBACK_CHAIN: &[(&str, &str)] = &[
    ("groq", "openai/gpt-oss-120b"),
    ("groq", "meta-llama/llama-4-scout-17b-16e-instruct"),
    ("groq", "llama-3.3-70b-versatile"),
    ("cerebras", "gpt-oss-120b"),
    ("cerebras", "zai-glm-4.7"),
    ("groq", "openai/gpt-oss-20b"),
    ("groq", "llama-3.1-8b-instant"),
];

/// When a paid-mode response reports fewer than this many tokens left in the
/// current window, cool down briefly so concurrent batches don't trip a 429.
const PAID_LOW_TOKEN_THRESHOLD: u64 = 4000;
const PAID_COOLDOWN_CAP_SECS: u64 = 15;

/// Execution profile for one provider + tier (free/paid). This is what makes
/// chunking/pacing dynamic and *per-provider*: the job's primary entry drives the
/// upfront, job-level knobs (`batch_multiplier`, `concurrency`, path), while each
/// individual request reads the profile of the model it actually hits for the
/// per-request knobs (`max_output_tokens`, `free_sleep_ms`). So a free-Cerebras
/// request keeps its small, 8K-context-safe cap even after the job has fallen
/// through to a paid Groq, and that paid Groq tail stops sleeping.
struct Profile {
    /// Multiplier applied to the per-format base batch size. Big on paid (exploit
    /// throughput), 1 on free (respect low rpm / 8K context).
    batch_multiplier: usize,
    /// `max_completion_tokens` reserved per request.
    max_output_tokens: u32,
    /// Bounded concurrency for the paid (parallel) path.
    concurrency: usize,
    /// Sleep after a request served by this profile on the free (sequential) path, ms.
    free_sleep_ms: u64,
}

/// Picks chunk scaling, output-token cap, concurrency and free-tier pacing for a
/// given `provider` + `paid` tier. Single source of truth; numbers are intentionally
/// easy to tune. Rationale per cell is in PLAN/Context.
fn profile_for(provider: &str, paid: bool) -> Profile {
    match (provider, paid) {
        // Paid Cerebras (Developer/PAYG): very high ceilings — gpt-oss-120b is
        // 1000 rpm / 1M tpm (zai-glm-4.7 500 rpm / 500K tpm), per Cerebras docs
        // (confirmed 2026-06-15). TPM is the binding limit for fat batches, so the
        // per-response token-budget backpressure (RateInfo cooldown) — not a fixed
        // sleep — does the throttling. concurrency 12 is a safe burst ceiling well
        // under the rpm cap; exploit it with fat batches + a big output cap.
        ("cerebras", true) => Profile {
            batch_multiplier: 3,
            max_output_tokens: 8000,
            concurrency: 12,
            free_sleep_ms: 0,
        },
        // Free-trial Cerebras: a hard 5 rpm / 30K tpm (1M tpd) on both models, per
        // Cerebras docs (confirmed 2026-06-15). At 5 rpm, parallelism is pointless
        // (concurrency 1) and the binding limit is requests, not tokens — so pace
        // one request per 12 s (60/5) to sit exactly on the ceiling. The loop sleeps
        // *between* batches, so 12000 ms + request latency stays at/under 5 rpm;
        // the prior 8000 ms ran ~6–7 rpm and tripped 429s.
        ("cerebras", false) => Profile {
            batch_multiplier: 1,
            max_output_tokens: 3000,
            concurrency: 1,
            free_sleep_ms: 12000,
        },
        // Paid Groq: higher than free but below paid Cerebras — moderate scaling.
        ("groq", true) => Profile {
            batch_multiplier: 2,
            max_output_tokens: 6000,
            concurrency: 8,
            free_sleep_ms: 0,
        },
        // Free Groq (and any unknown provider): preserve the prior conservative
        // behavior — format-native batch, 4096 cap, sequential, 1.5s spacing.
        _ => Profile {
            batch_multiplier: 1,
            max_output_tokens: 4096,
            concurrency: 1,
            free_sleep_ms: 1500,
        },
    }
}

/// Fetches a provider's available chat/text models for the settings UI. If `key`
/// is supplied it is used directly (lets the connect wizard validate a pasted key
/// before saving it); otherwise the saved key is read from secrets.
pub async fn list_models(
    app: &AppHandle,
    provider: &str,
    key: Option<String>,
) -> Result<Vec<ModelInfo>, String> {
    let key = match key {
        Some(k) if !k.trim().is_empty() => k,
        _ => crate::secrets::get_key(app, provider)
            .ok_or("לא הוגדר מפתח API לספק זה. הזן ושמור מפתח קודם.")?,
    };
    let client = reqwest::Client::new();
    provider::fetch_models(&client, provider, &key).await
}

/// One link in the translation fallback chain: a provider + model, plus whether
/// that provider's key is in paid mode (which selects the parallel execution path).
struct ChainEntry {
    provider: &'static str,
    model: String,
    paid: bool,
}

/// Builds the effective fallback chain: any user-selected models first (tried
/// first, paid flag from their provider), then the built-in defaults as backup.
/// Deduplicated by (provider, model).
fn build_chain(settings: &crate::settings::Settings) -> Vec<ChainEntry> {
    fn push(
        chain: &mut Vec<ChainEntry>,
        seen: &mut HashSet<(String, String)>,
        provider: &'static str,
        model: String,
        paid: bool,
    ) {
        if seen.insert((provider.to_string(), model.clone())) {
            chain.push(ChainEntry { provider, model, paid });
        }
    }

    let mut chain: Vec<ChainEntry> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    if let Some(m) = settings.groq_model.as_deref().filter(|s| !s.trim().is_empty()) {
        push(&mut chain, &mut seen, "groq", m.to_string(), settings.groq_paid);
    }
    if let Some(m) = settings
        .cerebras_model
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        push(&mut chain, &mut seen, "cerebras", m.to_string(), settings.cerebras_paid);
    }
    for (provider, model) in FALLBACK_CHAIN {
        let paid = match *provider {
            "groq" => settings.groq_paid,
            "cerebras" => settings.cerebras_paid,
            _ => false,
        };
        push(&mut chain, &mut seen, provider, model.to_string(), paid);
    }
    chain
}

/// API keys available for this translation job.
struct Keys {
    groq: Option<String>,
    cerebras: Option<String>,
}

impl Keys {
    fn for_provider(&self, provider: &str) -> Option<&str> {
        match provider {
            "groq" => self.groq.as_deref(),
            "cerebras" => self.cerebras.as_deref(),
            _ => None,
        }
    }
}

/// Result of translating a single batch through the chain. `map` is already
/// filtered to (and keyed by) the batch's own unit ids.
#[derive(Default)]
struct BatchOutcome {
    map: Option<HashMap<usize, String>>,
    last_error: Option<String>,
    /// Free-path pacing appropriate for the model that actually served this batch
    /// (`Some` only on success). Lets the sequential loop drop the primary's slow
    /// spacing once a paid provider takes over after a free primary is exhausted.
    served_sleep_ms: Option<u64>,
}

fn merge_outcome(
    outcome: BatchOutcome,
    translated: &mut HashMap<usize, String>,
    last_error: &mut Option<String>,
) {
    if let Some(m) = outcome.map {
        translated.extend(m);
    }
    if let Some(e) = outcome.last_error {
        *last_error = Some(e);
    }
}

const SYSTEM_PROMPT_SUBTITLE: &str = "You are a professional subtitle translator.\nYou receive a JSON object whose keys are subtitle IDs and values are subtitle texts.\nTranslate every value into {target_language}.\nRules:\n- Preserve every JSON key exactly as given.\n- Preserve line breaks ('\\n') inside a value at the same positions.\n- Keep leading speaker dashes ('-') if present.\n- Keep proper nouns of people and places as-is unless they have a well-known {target_language} form.\n- Use natural spoken tone suitable for subtitles.\n- Respond with ONLY the JSON object. No commentary, no markdown code fences.";

/// Appended to the subtitle system prompt only when at least one unit carries a
/// gender tag (see `gender_tags_for_chunks`). Phrased as likelihood, not fact —
/// the addressee heuristic is wrong in multi-speaker scenes (documented risk).
const GENDER_PROMPT_ADDENDUM: &str = "\nSome values begin with a speaker-gender tag: '[M]' = male speaker, '[F]' = female speaker.\nGender rules:\n- Inflect first-person verbs, adjectives and self-references according to the speaker's tagged gender.\n- When a tagged segment is adjacent to a segment with the opposite tag, it is likely a dialogue: second-person address ('you') in each segment is probably directed at the other speaker — inflect second-person forms to the other speaker's gender when the context supports it.\n- Untagged values have unknown speaker gender — translate them as you normally would.\n- NEVER include the tags '[M]' or '[F]' (or any bracketed gender marker) in the translated output.";

pub(crate) const SYSTEM_PROMPT_DOCUMENT: &str = "You are a professional document translator.\nYou receive a JSON object whose keys are paragraph IDs and values are source paragraphs.\nTranslate every value into {target_language}.\nRules:\n- Preserve every JSON key exactly as given.\n- Preserve line breaks ('\\n') and blank lines inside a value at the same positions.\n- Preserve markdown syntax (**bold**, *italic*, `code`, [links](...), # headings, lists, tables) exactly as-is.\n- Keep proper nouns of people, places, brands, and code identifiers as-is unless a well-known {target_language} form exists.\n- Use natural tone appropriate for the document.\n- Respond with ONLY the JSON object. No commentary, no markdown code fences.";

/// Builds the output path `<stem>.<lang>.<ext>` next to `basis`.
fn output_path_with_ext(basis: &Path, target_language: &str, ext: &str) -> PathBuf {
    let suffix = target_language.to_lowercase().replace(' ', "_");
    let stem = basis
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    basis.with_file_name(format!("{stem}.{suffix}.{ext}"))
}

/// Matches the sidecar's labeled time windows against the document's subtitle
/// chunks: a chunk is tagged when one entry covers ≥50% of its duration. The
/// sidecar was written from the very cue list the SRT was rendered from, so an
/// unedited file matches exactly; a hand-edited one degrades to fewer tags,
/// never to wrong ones (overlap-gated).
fn gender_tags_for_chunks(
    doc: &parser::ParsedDoc,
    entries: &[crate::gender_f0::CueGender],
) -> HashMap<usize, &'static str> {
    let mut tags = HashMap::new();
    for chunk in doc.chunks.iter().filter(|c| c.translatable) {
        let Some((t0, t1)) = chunk.time_ms() else { continue };
        let (t0, t1) = (t0 as i64, t1 as i64);
        let dur = t1 - t0;
        if dur <= 0 {
            continue;
        }
        let best = entries
            .iter()
            .map(|e| {
                let overlap = (t1.min(e.t1_cs * 10) - t0.max(e.t0_cs * 10)).max(0);
                (overlap, e.gender)
            })
            .max_by_key(|(overlap, _)| *overlap);
        if let Some((overlap, gender)) = best {
            if overlap * 2 >= dur {
                let g = match gender {
                    crate::gender_f0::SegmentGender::Male => "M",
                    crate::gender_f0::SegmentGender::Female => "F",
                    crate::gender_f0::SegmentGender::Unknown => continue,
                };
                tags.insert(chunk.id, g);
            }
        }
    }
    tags
}

/// Removes every `[M]`/`[F]` speaker tag (and the space gluing it to a word)
/// from translated text — the defensive layer behind the prompt's "never echo
/// the tags" rule.
fn strip_gender_tags(s: &str) -> String {
    let mut out = s.to_string();
    for tag in ["[M] ", "[F] ", " [M]", " [F]", "[M]", "[F]"] {
        if out.contains(tag) {
            out = out.replace(tag, "");
        }
    }
    out
}

/// Removes Unicode directional formatting marks (LRM/RLM, embeddings, overrides,
/// isolates) and the zero-width no-break space / BOM from a string, leaving plain
/// logical-order text. Models translating to Hebrew/Arabic often inject these into
/// subtitle text; VLC renders RLM/LRM as a stray on-screen glyph (vlc#13059) and a
/// bidi-capable player lays RTL out correctly without them — the same "plain logical
/// order, no marks" guarantee the video→SRT pipeline relies on. Real punctuation
/// (including an intentional leading `...`) is preserved.
fn strip_directional_marks(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                *c,
                '\u{200E}' | '\u{200F}'        // LRM, RLM
                    | '\u{202A}'..='\u{202E}'  // LRE, RLE, PDF, LRO, RLO
                    | '\u{2066}'..='\u{2069}'  // LRI, RLI, FSI, PDI
                    | '\u{FEFF}'               // BOM / zero-width no-break space
            )
        })
        .collect()
}

/// Normalizes a translated subtitle value: trims surrounding whitespace and drops
/// blank / whitespace-only lines. A model that collapses a multi-line cue onto one
/// line can leave a trailing `\n` (empty second line); rendered into SRT that
/// becomes a stray blank line which splits/renumbers the cue in strict parsers.
/// Dropping empty lines keeps a real `\n` line break but never an empty one.
fn normalize_subtitle_value(s: &str) -> String {
    s.lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod gender_tag_tests {
    use super::{gender_tags_for_chunks, strip_gender_tags, parser};
    use crate::gender_f0::{CueGender, SegmentGender};

    #[test]
    fn leaked_tags_are_stripped_from_output() {
        assert_eq!(strip_gender_tags("[F] את יודעת"), "את יודעת");
        assert_eq!(strip_gender_tags("[M] אתה יודע"), "אתה יודע");
        assert_eq!(strip_gender_tags("שלום [F] עולם"), "שלום עולם");
        assert_eq!(strip_gender_tags("בלי תגית"), "בלי תגית");
        // Multi-line subtitle value with a tag on each line.
        assert_eq!(strip_gender_tags("[F] שורה\n[M] שנייה"), "שורה\nשנייה");
    }

    #[test]
    fn chunks_match_sidecar_by_time_overlap() {
        let doc = parser::parse(
            parser::Format::Srt,
            "1\n00:00:00,000 --> 00:00:02,000\nHello\n\n\
             2\n00:00:02,500 --> 00:00:04,000\nHi there\n\n\
             3\n00:00:10,000 --> 00:00:12,000\nLater\n\n",
        );
        let entries = vec![
            // Exact match for cue 1 (cs units).
            CueGender { t0_cs: 0, t1_cs: 200, gender: SegmentGender::Male },
            // Exact match for cue 2.
            CueGender { t0_cs: 250, t1_cs: 400, gender: SegmentGender::Female },
            // Covers <50% of cue 3 → must NOT tag it.
            CueGender { t0_cs: 1000, t1_cs: 1050, gender: SegmentGender::Male },
        ];
        let tags = gender_tags_for_chunks(&doc, &entries);
        assert_eq!(tags.get(&1), Some(&"M"));
        assert_eq!(tags.get(&2), Some(&"F"));
        assert_eq!(tags.get(&3), None, "sub-50% overlap must stay untagged");
    }

    #[test]
    fn no_entries_means_no_tags() {
        let doc = parser::parse(
            parser::Format::Srt,
            "1\n00:00:00,000 --> 00:00:02,000\nHello\n\n",
        );
        assert!(gender_tags_for_chunks(&doc, &[]).is_empty());
    }
}

#[cfg(test)]
mod rtl_subtitle_tests {
    use super::{finalize_output, strip_directional_marks, Category};

    #[test]
    fn strips_all_bidi_marks_but_keeps_text_and_punctuation() {
        // Leading RLM + an RLE…PDF wrap + a trailing LRM, around real text whose
        // leading "..." continuation marker MUST survive.
        let input = "\u{200F}\u{202B}...\u{202C}שלום, עולם.\u{200E}";
        assert_eq!(strip_directional_marks(input), "...שלום, עולם.");
        for m in ['\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{2066}', '\u{FEFF}'] {
            assert!(!strip_directional_marks(input).contains(m), "mark survived");
        }
    }

    #[test]
    fn plain_hebrew_is_unchanged() {
        let s = "שלום לכם, מה שלומכם?\nשורה שנייה.";
        assert_eq!(strip_directional_marks(s), s);
    }

    #[test]
    fn subtitle_output_gets_utf8_bom_documents_do_not() {
        let srt = "1\n00:00:00,000 --> 00:00:02,000\nשלום\n";
        let with_bom = finalize_output(Category::Subtitle, srt.to_string());
        assert!(with_bom.starts_with('\u{FEFF}'), "subtitle output must start with a BOM");
        assert_eq!(&with_bom[3..], srt, "BOM is 3 bytes, content unchanged after it");
        // Encoded bytes start with the canonical UTF-8 BOM EF BB BF.
        assert_eq!(&with_bom.as_bytes()[..3], &[0xEF, 0xBB, 0xBF]);

        let txt = finalize_output(Category::Document, "טקסט רגיל".to_string());
        assert!(!txt.starts_with('\u{FEFF}'), "document output must stay BOM-less");
    }

    #[test]
    fn srt_through_parse_render_pipeline_ends_with_bom() {
        // The same shape `translate_text_format` runs (minus the LLM): a real SRT
        // through the structural parser, rendered with no translations, finalized.
        // Proves Format::Srt → Category::Subtitle → BOM, byte-for-byte.
        let doc = super::parser::parse(
            super::parser::Format::Srt,
            "1\n00:00:00,000 --> 00:00:02,000\nשלום עולם\n\n",
        );
        let out = finalize_output(doc.category(), doc.render(&std::collections::HashMap::new()));
        assert_eq!(&out.as_bytes()[..3], &[0xEF, 0xBB, 0xBF], "missing UTF-8 BOM");
        assert!(out.contains("שלום עולם"), "content lost:\n{out}");
        assert!(out.contains("00:00:00,000 --> 00:00:02,000"), "timing lost:\n{out}");
    }

    #[test]
    fn bom_roundtrip_translated_file_is_readable_as_input() {
        // A translated SRT (with BOM) fed back into the translator's own reader
        // must parse identically — the input path strips the BOM (see
        // `translate_text_format`), mirrored here.
        let out = finalize_output(Category::Subtitle, "1\n00:00:00,000 --> 00:00:01,000\nא\n".into());
        let reread = out.strip_prefix('\u{FEFF}').map(str::to_string).unwrap_or(out);
        assert!(reread.starts_with('1'), "BOM must strip cleanly on re-read");
    }
}

/// Entry point: dispatches on file extension.
pub async fn translate_file(app: &AppHandle, path: &str) -> Result<String, String> {
    let target = crate::settings::load_or_init(app)?.translate_target_language;
    translate_file_to(app, path, &target).await
}

/// Like [`translate_file`] but with an explicit target language. Used by the video
/// "transcribe + translate" drop flow, where the user picks the language in the
/// chooser instead of relying on the saved default.
pub async fn translate_file_to(app: &AppHandle, path: &str, target: &str) -> Result<String, String> {
    let input = PathBuf::from(path);
    if !input.exists() {
        return Err(format!("הקובץ לא נמצא: {path}"));
    }

    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "docx" => docx::translate_docx(app, &input, &input, target).await,
        "pdf" => pdf::translate_pdf(app, &input, target).await,
        "doc" => doc_via_libreoffice(app, &input, target).await,
        _ => translate_text_format(app, &input, &ext, target).await,
    }
}

/// Subtitle/plain-text path (SRT/VTT/SBV/TXT/MD) via the structural parser.
async fn translate_text_format(
    app: &AppHandle,
    input: &Path,
    ext: &str,
    target: &str,
) -> Result<String, String> {
    let format = parser::Format::from_ext(ext).ok_or_else(|| {
        format!("פורמט לא נתמך: .{ext}. נתמכים: srt, vtt, sbv, txt, md, docx, pdf, doc")
    })?;

    let raw = std::fs::read_to_string(input).map_err(|e| format!("שגיאה בקריאת הקובץ: {e}"))?;
    let content = raw.strip_prefix('\u{FEFF}').map(str::to_string).unwrap_or(raw);

    let doc = parser::parse(format, &content);
    if doc.chunks.is_empty() {
        return Err("לא נמצא תוכן לתרגום בקובץ".into());
    }

    // Speaker-gender tags (opt-in, experimental): when a `<stem>.genders.json`
    // sidecar sits next to the subtitle (written by the video→SRT pipeline) and
    // the target is gendered (Hebrew in V1), tagged cues are prefixed `[M]`/`[F]`
    // so the model picks correct gender inflections. No sidecar / feature off /
    // other target ⇒ empty map ⇒ behavior identical to today.
    let gender_tags: HashMap<usize, &'static str> = if matches!(doc.category(), Category::Subtitle)
        && target.eq_ignore_ascii_case("hebrew")
        && crate::settings::load_or_init(app)?.gender_aware_translation
    {
        crate::gender_f0::load_for_srt(input)
            .map(|entries| gender_tags_for_chunks(&doc, &entries))
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    let mut system_prompt = match doc.category() {
        Category::Subtitle => SYSTEM_PROMPT_SUBTITLE,
        Category::Document => SYSTEM_PROMPT_DOCUMENT,
    }
    .to_string();
    if !gender_tags.is_empty() {
        system_prompt.push_str(GENDER_PROMPT_ADDENDUM);
    }
    let batch_size = doc.batch_size();

    let units: Vec<(usize, String)> = doc
        .chunks
        .iter()
        .filter(|c| c.translatable && !c.text.trim().is_empty())
        .map(|c| {
            let text = match gender_tags.get(&c.id) {
                Some(g) => format!("[{g}] {}", c.text),
                None => c.text.clone(),
            };
            (c.id, text)
        })
        .collect();

    let out = output_path_with_ext(input, target, ext);

    let mut map = if units.is_empty() {
        HashMap::new()
    } else {
        translate_units(app, target, &system_prompt, batch_size, &units).await?
    };

    // Subtitle output must be plain logical-order text so it renders RTL correctly:
    // strip any directional marks the model injected (see `strip_directional_marks`).
    // Mirrors the video→SRT pipeline; harmless for LTR targets (no marks to remove).
    if matches!(doc.category(), Category::Subtitle) {
        for v in map.values_mut() {
            *v = normalize_subtitle_value(&strip_directional_marks(v));
        }
    }

    // Defensive tag strip: the prompt forbids echoing `[M]`/`[F]`, but a model
    // may still leak one — they must never reach the output file.
    if !gender_tags.is_empty() {
        for v in map.values_mut() {
            *v = strip_gender_tags(v);
        }
    }

    let rendered = finalize_output(doc.category(), doc.render(&map));
    std::fs::write(&out, rendered).map_err(|e| format!("שגיאה בכתיבת הפלט: {e}"))?;
    Ok(out.to_string_lossy().into_owned())
}

/// Final encoding shape of a translated text file. Subtitle files get a UTF-8
/// BOM: they are consumed by media players, several of which (Windows Media
/// Player, TVs, older desktop players) misdetect BOM-less UTF-8 Hebrew as ANSI
/// and render gibberish — the BOM pins the encoding. The BOM is a *file*
/// prefix, not text: every reader in this codebase (the translation input at
/// the top of `translate_text_format`, `subtitle_burn::srt_parse`) already
/// strips it. TXT/MD outputs stay BOM-less (editor- and tool-friendly).
fn finalize_output(category: Category, mut rendered: String) -> String {
    if matches!(category, Category::Subtitle) {
        rendered.insert(0, '\u{FEFF}');
    }
    rendered
}

/// Legacy `.doc`: convert to a temporary `.docx` via LibreOffice, translate that,
/// and write the result next to the original `.doc`.
async fn doc_via_libreoffice(
    app: &AppHandle,
    input: &Path,
    target: &str,
) -> Result<String, String> {
    let tmp_dir = std::env::temp_dir().join(format!("timluli-doc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;

    let input_owned = input.to_path_buf();
    let tmp_owned = tmp_dir.clone();
    let convert = tokio::task::spawn_blocking(move || run_soffice_convert(&input_owned, &tmp_owned))
        .await
        .map_err(|e| e.to_string())?;

    let converted = match convert {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }
    };

    let result = docx::translate_docx(app, &converted, input, target).await;
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

/// Runs LibreOffice headless to convert `input` (.doc) into `<out_dir>/<stem>.docx`.
fn run_soffice_convert(input: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let candidates = [
        "soffice",
        r"C:\Program Files\LibreOffice\program\soffice.exe",
        r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
    ];
    let mut ran = false;
    for exe in candidates {
        let status = std::process::Command::new(exe)
            .args(["--headless", "--convert-to", "docx", "--outdir"])
            .arg(out_dir)
            .arg(input)
            .status();
        if let Ok(s) = status {
            ran = true;
            if s.success() {
                let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
                let converted = out_dir.join(format!("{stem}.docx"));
                if converted.exists() {
                    return Ok(converted);
                }
            }
        }
    }
    if ran {
        Err("המרת ה-.doc נכשלה. שמור את הקובץ כ-DOCX ונסה שוב.".into())
    } else {
        Err("תרגום קובצי .doc (פורמט ישן) דורש LibreOffice מותקן. שמור את הקובץ כ-DOCX ונסה שוב.".into())
    }
}

/// Shared batch/fallback translation loop. Takes translatable `(id, text)` units
/// and returns a map of id -> translated text. Errors only if nothing translated.
pub(crate) async fn translate_units(
    app: &AppHandle,
    target: &str,
    system_prompt: &str,
    batch_size: usize,
    units: &[(usize, String)],
) -> Result<HashMap<usize, String>, String> {
    let settings = crate::settings::load_or_init(app)?;
    let keys = Keys {
        groq: crate::secrets::get_key(app, "groq"),
        cerebras: crate::secrets::get_key(app, "cerebras"),
    };
    if keys.groq.is_none() && keys.cerebras.is_none() {
        return Err(
            "לא הוגדרו מפתחות API. הוסף מפתח Groq או Cerebras בהגדרות → תרגום מסמכים.".into(),
        );
    }
    if units.is_empty() {
        return Ok(HashMap::new());
    }

    let chain = build_chain(&settings);
    // Path selection: the first chain entry whose provider key exists is the
    // primary model; its provider + paid flag drive the execution profile
    // (chunk size, output cap, concurrency, pacing) and parallel-vs-sequential.
    let primary = chain.iter().find(|e| keys.for_provider(e.provider).is_some());
    let primary_paid = primary.map(|e| e.paid).unwrap_or(false);
    let primary_profile = profile_for(primary.map(|e| e.provider).unwrap_or("groq"), primary_paid);
    // Chunking is decided once, upfront, sized for the primary (the provider doing
    // most of the work). Per-request output cap + free-path pacing adapt per entry
    // inside `run_batch` (see `BatchOutcome::served_sleep_ms`).
    let batch_size = batch_size.saturating_mul(primary_profile.batch_multiplier).max(1);

    let total_batches = units.len().div_ceil(batch_size);
    let client = reqwest::Client::new();
    let exhausted: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

    let mut translated: HashMap<usize, String> = HashMap::new();
    let mut last_error: Option<String> = None;

    if primary_paid {
        // Paid path: run batches concurrently (bounded), letting per-response token
        // budget cool-downs throttle so we ride the high paid limits without 429s.
        let concurrency = primary_profile.concurrency.min(total_batches).max(1);
        let completed = AtomicUsize::new(0);
        // Drive per-batch futures with bounded concurrency. Each closure takes an
        // owned (start,end) range and borrows `units` from the function scope (not
        // the closure argument), which avoids a higher-ranked-lifetime limitation
        // that arises when an async block borrows the closure's own parameter.
        let ranges: Vec<(usize, usize)> = (0..total_batches)
            .map(|i| (i * batch_size, ((i + 1) * batch_size).min(units.len())))
            .collect();
        let futures = ranges.into_iter().map(|(start, end)| {
            let client = &client;
            let chain = &chain;
            let keys = &keys;
            let exhausted = &exhausted;
            let completed = &completed;
            async move {
                let pairs: Vec<(usize, &str)> =
                    units[start..end].iter().map(|(id, t)| (*id, t.as_str())).collect();
                let outcome = run_batch(
                    client,
                    chain,
                    keys,
                    target,
                    system_prompt,
                    &pairs,
                    exhausted,
                    true,
                )
                .await;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                let progress = serde_json::json!({ "batch": done, "total": total_batches });
                let _ = app.emit_to("mic", "speakly://translate-progress", progress.clone());
                let _ = app.emit_to("panel", "speakly://translate-progress", progress);
                outcome
            }
        });
        let outcomes: Vec<BatchOutcome> = stream::iter(futures)
            .buffer_unordered(concurrency)
            .collect()
            .await;

        for outcome in outcomes {
            merge_outcome(outcome, &mut translated, &mut last_error);
        }
    } else {
        // Conservative free-tier path: strictly sequential with a fixed sleep
        // between batches to stay under the per-minute limits.
        for (bi, batch) in units.chunks(batch_size).enumerate() {
            let progress = serde_json::json!({ "batch": bi + 1, "total": total_batches });
            let _ = app.emit_to("mic", "speakly://translate-progress", progress.clone());
            let _ = app.emit_to("panel", "speakly://translate-progress", progress);
            let pairs: Vec<(usize, &str)> = batch.iter().map(|(id, t)| (*id, t.as_str())).collect();
            let outcome = run_batch(
                &client,
                &chain,
                &keys,
                target,
                system_prompt,
                &pairs,
                &exhausted,
                false,
            )
            .await;
            // Pace by whoever actually served this batch: once a free primary is
            // exhausted and a paid provider takes over, its `free_sleep_ms` is 0,
            // so the tail stops crawling at the primary's spacing. Falls back to the
            // primary's pacing if the batch failed entirely.
            let sleep_ms = outcome
                .served_sleep_ms
                .unwrap_or(primary_profile.free_sleep_ms);
            merge_outcome(outcome, &mut translated, &mut last_error);

            if bi + 1 < total_batches && sleep_ms > 0 {
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            }
        }
    }

    if translated.is_empty() {
        return Err(format!(
            "התרגום נכשל — בדוק את מפתחות ה-API והחיבור לאינטרנט. {}",
            last_error.unwrap_or_else(|| "שגיאה לא ידועה".into())
        ));
    }
    Ok(translated)
}

/// Translates one batch by walking the fallback chain. `exhausted` is shared
/// across batches (and across concurrent tasks in the paid path) so a quota- or
/// rate-exhausted model is skipped cheaply afterward. Returns the batch's
/// translations keyed by their own unit ids.
#[allow(clippy::too_many_arguments)]
async fn run_batch(
    client: &reqwest::Client,
    chain: &[ChainEntry],
    keys: &Keys,
    target: &str,
    system_prompt: &str,
    pairs: &[(usize, &str)],
    exhausted: &Mutex<HashSet<String>>,
    paid: bool,
) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();
    let mut attempt = 0usize;
    let mut rate_retries = 0u32;

    while attempt < chain.len() {
        let entry = &chain[attempt];
        let label = format!("{}:{}", entry.provider, entry.model);
        if exhausted.lock().contains(&label) {
            attempt += 1;
            continue;
        }
        let Some(key) = keys.for_provider(entry.provider) else {
            exhausted.lock().insert(label);
            attempt += 1;
            continue;
        };

        // Per-entry profile: each request's output cap (and the pacing it implies)
        // matches the model actually being hit, not the job's primary — so a free
        // Cerebras request stays 8K-safe and a paid fallback isn't throttled.
        let entry_profile = profile_for(entry.provider, entry.paid);

        match provider::translate_batch(
            client,
            provider::base_url(entry.provider),
            &entry.model,
            key,
            target,
            system_prompt,
            pairs,
            entry_profile.max_output_tokens,
        )
        .await
        {
            Ok((m, rate)) => {
                let mut out: HashMap<usize, String> = HashMap::new();
                for (id, _t) in pairs {
                    if let Some(tx) = m.get(&id.to_string()) {
                        out.insert(*id, tx.clone());
                    }
                }
                outcome.map = Some(out);
                outcome.served_sleep_ms = Some(entry_profile.free_sleep_ms);
                // Paid path backpressure: if the remaining per-minute token budget
                // is low, cool down briefly so concurrent batches don't trip a 429.
                if paid {
                    if let Some(rem) = rate.remaining_tokens {
                        if rem < PAID_LOW_TOKEN_THRESHOLD {
                            let wait =
                                rate.reset_secs.unwrap_or(2).clamp(1, PAID_COOLDOWN_CAP_SECS);
                            tokio::time::sleep(Duration::from_secs(wait)).await;
                        }
                    }
                }
                return outcome;
            }
            Err(provider::TranslateError::RateLimit(msg, retry_after)) => {
                outcome.last_error = Some(msg);
                // Per-minute rate limit: wait and retry the SAME model a few times
                // before giving up on it (the window resets quickly).
                if rate_retries < 3 {
                    rate_retries += 1;
                    let wait = retry_after.unwrap_or(12).clamp(2, 30);
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                } else {
                    exhausted.lock().insert(label);
                    attempt += 1;
                }
            }
            Err(provider::TranslateError::Quota(msg)) => {
                outcome.last_error = Some(msg);
                exhausted.lock().insert(label);
                attempt += 1;
            }
            Err(provider::TranslateError::Transient(msg)) => {
                outcome.last_error = Some(msg);
                attempt += 1;
            }
        }
    }
    outcome
}
