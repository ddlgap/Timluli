//! Document translation engine (Rust port of cerabras/translate_drop.py).
//! Parses a subtitle/text file into chunks, translates the translatable chunks
//! batch-by-batch through a provider fallback chain, then re-renders a copy next
//! to the original. Provider HTTP goes out from Rust (reqwest), so it is not
//! subject to the webview CSP.

mod parser;
mod provider;

use parser::Category;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const FALLBACK_CHAIN: &[(&str, &str)] = &[
    ("groq", "llama-3.3-70b-versatile"),
    ("groq", "openai/gpt-oss-120b"),
    ("groq", "llama-3.1-8b-instant"),
    ("cerebras", "qwen-3-235b-a22b-instruct-2507"),
    ("cerebras", "gpt-oss-120b"),
    ("cerebras", "llama3.1-8b"),
];

const SLEEP_BETWEEN_BATCHES_MS: u64 = 1500;

const SYSTEM_PROMPT_SUBTITLE: &str = "You are a professional subtitle translator.\nYou receive a JSON object whose keys are subtitle IDs and values are subtitle texts.\nTranslate every value into {target_language}.\nRules:\n- Preserve every JSON key exactly as given.\n- Preserve line breaks ('\\n') inside a value at the same positions.\n- Keep leading speaker dashes ('-') if present.\n- Keep proper nouns of people and places as-is unless they have a well-known {target_language} form.\n- Use natural spoken tone suitable for subtitles.\n- Respond with ONLY the JSON object. No commentary, no markdown code fences.";

const SYSTEM_PROMPT_DOCUMENT: &str = "You are a professional document translator.\nYou receive a JSON object whose keys are paragraph IDs and values are source paragraphs.\nTranslate every value into {target_language}.\nRules:\n- Preserve every JSON key exactly as given.\n- Preserve line breaks ('\\n') and blank lines inside a value at the same positions.\n- Preserve markdown syntax (**bold**, *italic*, `code`, [links](...), # headings, lists, tables) exactly as-is.\n- Keep proper nouns of people, places, brands, and code identifiers as-is unless a well-known {target_language} form exists.\n- Use natural tone appropriate for the document.\n- Respond with ONLY the JSON object. No commentary, no markdown code fences.";

fn output_path(input: &Path, target_language: &str) -> PathBuf {
    let suffix = target_language.to_lowercase().replace(' ', "_");
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("txt");
    input.with_file_name(format!("{stem}.{suffix}.{ext}"))
}

/// Translates `path` and writes `<stem>.<lang>.<ext>` beside it.
/// Returns the output path on success.
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
    let format = parser::Format::from_ext(&ext)
        .ok_or_else(|| format!("פורמט לא נתמך: .{ext}. נתמכים: srt, vtt, sbv, txt, md"))?;

    let raw = std::fs::read_to_string(&input).map_err(|e| format!("שגיאה בקריאת הקובץ: {e}"))?;
    let content = raw.strip_prefix('\u{FEFF}').map(str::to_string).unwrap_or(raw);

    let doc = parser::parse(format, &content);
    if doc.chunks.is_empty() {
        return Err("לא נמצא תוכן לתרגום בקובץ".into());
    }

    let stg = crate::settings::load_or_init(app)?;
    let target = stg.translate_target_language;

    let groq_key = crate::secrets::get_key(app, "groq");
    let cerebras_key = crate::secrets::get_key(app, "cerebras");
    if groq_key.is_none() && cerebras_key.is_none() {
        return Err(
            "לא הוגדרו מפתחות API. הוסף מפתח Groq או Cerebras בהגדרות → תרגום מסמכים.".into(),
        );
    }

    let system_prompt = match doc.category() {
        Category::Subtitle => SYSTEM_PROMPT_SUBTITLE,
        Category::Document => SYSTEM_PROMPT_DOCUMENT,
    };
    let batch_size = doc.batch_size();

    let translatable: Vec<&parser::Chunk> = doc
        .chunks
        .iter()
        .filter(|c| c.translatable && !c.text.trim().is_empty())
        .collect();

    if translatable.is_empty() {
        // Nothing to translate (e.g. an all-code-block markdown) — copy as-is.
        let out_path = output_path(&input, &target);
        let rendered = doc.render(&HashMap::new());
        std::fs::write(&out_path, rendered).map_err(|e| format!("שגיאה בכתיבת הפלט: {e}"))?;
        return Ok(out_path.to_string_lossy().into_owned());
    }

    let total_batches = translatable.len().div_ceil(batch_size);
    let client = reqwest::Client::new();

    let mut translated: HashMap<usize, String> = HashMap::new();
    let mut exhausted: HashSet<String> = HashSet::new();
    let mut current_idx = 0usize;
    let mut last_error: Option<String> = None;

    for (bi, batch) in translatable.chunks(batch_size).enumerate() {
        let _ = app.emit_to(
            "mic",
            "speakly://translate-progress",
            serde_json::json!({ "batch": bi + 1, "total": total_batches }),
        );

        let pairs: Vec<(usize, &str)> = batch.iter().map(|c| (c.id, c.text.as_str())).collect();

        let mut attempt = current_idx;
        let mut got: Option<HashMap<String, String>> = None;
        while attempt < FALLBACK_CHAIN.len() {
            let (provider_name, model) = FALLBACK_CHAIN[attempt];
            let label = format!("{provider_name}:{model}");
            if exhausted.contains(&label) {
                attempt += 1;
                continue;
            }
            let key = match provider_name {
                "groq" => groq_key.as_deref(),
                "cerebras" => cerebras_key.as_deref(),
                _ => None,
            };
            let Some(key) = key else {
                exhausted.insert(label);
                attempt += 1;
                continue;
            };

            match provider::translate_batch(
                &client,
                provider::base_url(provider_name),
                model,
                key,
                &target,
                system_prompt,
                &pairs,
            )
            .await
            {
                Ok(map) => {
                    got = Some(map);
                    current_idx = attempt;
                    break;
                }
                Err(provider::TranslateError::Quota(msg)) => {
                    last_error = Some(msg);
                    exhausted.insert(label);
                    attempt += 1;
                }
                Err(provider::TranslateError::Transient(msg)) => {
                    last_error = Some(msg);
                    attempt += 1;
                }
            }
        }

        if let Some(map) = got {
            for c in batch {
                if let Some(t) = map.get(&c.id.to_string()) {
                    translated.insert(c.id, t.clone());
                }
            }
        }

        if bi + 1 < total_batches {
            tokio::time::sleep(Duration::from_millis(SLEEP_BETWEEN_BATCHES_MS)).await;
        }
    }

    if translated.is_empty() {
        return Err(format!(
            "התרגום נכשל — בדוק את מפתחות ה-API והחיבור לאינטרנט. {}",
            last_error.unwrap_or_else(|| "שגיאה לא ידועה".into())
        ));
    }

    let out_path = output_path(&input, &target);
    let rendered = doc.render(&translated);
    std::fs::write(&out_path, rendered).map_err(|e| format!("שגיאה בכתיבת הפלט: {e}"))?;
    Ok(out_path.to_string_lossy().into_owned())
}
