//! Document translation engine (Rust port of cerabras/translate_drop.py).
//! Parses a subtitle/text/office file into translatable units, translates them
//! batch-by-batch through a provider fallback chain, then writes a translated
//! copy next to the original. Provider HTTP goes out from Rust (reqwest), so it
//! is not subject to the webview CSP.

mod docx;
mod parser;
mod pdf;
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
    let groq_key = crate::secrets::get_key(app, "groq");
    let cerebras_key = crate::secrets::get_key(app, "cerebras");
    if groq_key.is_none() && cerebras_key.is_none() {
        return Err(
            "לא הוגדרו מפתחות API. הוסף מפתח Groq או Cerebras בהגדרות → תרגום מסמכים.".into(),
        );
    }
    if units.is_empty() {
        return Ok(HashMap::new());
    }

    let total_batches = units.len().div_ceil(batch_size);
    let client = reqwest::Client::new();

    let mut translated: HashMap<usize, String> = HashMap::new();
    let mut exhausted: HashSet<String> = HashSet::new();
    let mut current_idx = 0usize;
    let mut last_error: Option<String> = None;

    for (bi, batch) in units.chunks(batch_size).enumerate() {
        let _ = app.emit_to(
            "mic",
            "speakly://translate-progress",
            serde_json::json!({ "batch": bi + 1, "total": total_batches }),
        );

        let pairs: Vec<(usize, &str)> = batch.iter().map(|(id, t)| (*id, t.as_str())).collect();

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
                target,
                system_prompt,
                &pairs,
            )
            .await
            {
                Ok(m) => {
                    got = Some(m);
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

        if let Some(m) = got {
            for (id, _t) in batch {
                if let Some(tx) = m.get(&id.to_string()) {
                    translated.insert(*id, tx.clone());
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
    Ok(translated)
}
