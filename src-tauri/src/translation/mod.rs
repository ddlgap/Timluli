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

const FALLBACK_CHAIN: &[(&str, &str)] = &[
    ("groq", "llama-3.3-70b-versatile"),
    ("groq", "openai/gpt-oss-120b"),
    ("groq", "llama-3.1-8b-instant"),
    ("cerebras", "qwen-3-235b-a22b-instruct-2507"),
    ("cerebras", "gpt-oss-120b"),
    ("cerebras", "llama3.1-8b"),
];

const SLEEP_BETWEEN_BATCHES_MS: u64 = 1500;

/// Paid-mode parallelism cap. The binding provider constraint is tokens-per-minute,
/// not requests, so we keep concurrency modest and let the adaptive token-budget
/// check below throttle further when the remaining TPM budget runs low.
const PAID_MAX_CONCURRENCY: usize = 8;
/// When a paid-mode response reports fewer than this many tokens left in the
/// current window, cool down briefly so concurrent batches don't trip a 429.
const PAID_LOW_TOKEN_THRESHOLD: u64 = 4000;
const PAID_COOLDOWN_CAP_SECS: u64 = 15;

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

/// Entry point: dispatches on file extension.
pub async fn translate_file(app: &AppHandle, path: &str) -> Result<String, String> {
    let input = PathBuf::from(path);
    if !input.exists() {
        return Err(format!("הקובץ לא נמצא: {path}"));
    }

    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let target = crate::settings::load_or_init(app)?.translate_target_language;

    match ext.as_str() {
        "docx" => docx::translate_docx(app, &input, &input, &target).await,
        "pdf" => pdf::translate_pdf(app, &input, &target).await,
        "doc" => doc_via_libreoffice(app, &input, &target).await,
        _ => translate_text_format(app, &input, &ext, &target).await,
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

    let system_prompt = match doc.category() {
        Category::Subtitle => SYSTEM_PROMPT_SUBTITLE,
        Category::Document => SYSTEM_PROMPT_DOCUMENT,
    };
    let batch_size = doc.batch_size();

    let units: Vec<(usize, String)> = doc
        .chunks
        .iter()
        .filter(|c| c.translatable && !c.text.trim().is_empty())
        .map(|c| (c.id, c.text.clone()))
        .collect();

    let out = output_path_with_ext(input, target, ext);

    let map = if units.is_empty() {
        HashMap::new()
    } else {
        translate_units(app, target, system_prompt, batch_size, &units).await?
    };

    std::fs::write(&out, doc.render(&map)).map_err(|e| format!("שגיאה בכתיבת הפלט: {e}"))?;
    Ok(out.to_string_lossy().into_owned())
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
    // primary model; its paid flag decides parallel-vs-sequential execution.
    let primary_paid = chain
        .iter()
        .find(|e| keys.for_provider(e.provider).is_some())
        .map(|e| e.paid)
        .unwrap_or(false);

    let total_batches = units.len().div_ceil(batch_size);
    let client = reqwest::Client::new();
    let exhausted: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

    let mut translated: HashMap<usize, String> = HashMap::new();
    let mut last_error: Option<String> = None;

    if primary_paid {
        // Paid path: run batches concurrently (bounded), letting per-response token
        // budget cool-downs throttle so we ride the high paid limits without 429s.
        let concurrency = PAID_MAX_CONCURRENCY.min(total_batches).max(1);
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
                let outcome =
                    run_batch(client, chain, keys, target, system_prompt, &pairs, exhausted, true)
                        .await;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = app.emit_to(
                    "mic",
                    "speakly://translate-progress",
                    serde_json::json!({ "batch": done, "total": total_batches }),
                );
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
            let _ = app.emit_to(
                "mic",
                "speakly://translate-progress",
                serde_json::json!({ "batch": bi + 1, "total": total_batches }),
            );
            let pairs: Vec<(usize, &str)> = batch.iter().map(|(id, t)| (*id, t.as_str())).collect();
            let outcome = run_batch(
                &client, &chain, &keys, target, system_prompt, &pairs, &exhausted, false,
            )
            .await;
            merge_outcome(outcome, &mut translated, &mut last_error);

            if bi + 1 < total_batches {
                tokio::time::sleep(Duration::from_millis(SLEEP_BETWEEN_BATCHES_MS)).await;
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

        match provider::translate_batch(
            client,
            provider::base_url(entry.provider),
            &entry.model,
            key,
            target,
            system_prompt,
            pairs,
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
