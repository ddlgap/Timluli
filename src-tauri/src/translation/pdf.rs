//! PDF adapter.
//!
//! Hebrew target → layout-preserving PDF→PDF via the `timluli-pdf` sidecar
//! (PyMuPDF + python-bidi, bundled as a Tauri resource and launched like the
//! Chrome/LibreOffice sidecars). Any other target → the legacy PDF→DOCX path
//! (`pdf_to_docx_fallback`), since the RTL rendering only fits Hebrew.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use docx_rust::document::{Paragraph, Run};
use docx_rust::formatting::{Bidi, CharacterProperty, ParagraphProperty, RightToLeftText};
use docx_rust::Docx;
use tauri::{AppHandle, Emitter, Manager};

pub async fn translate_pdf(app: &AppHandle, input: &Path, target: &str) -> Result<String, String> {
    // PDF→PDF (layout-preserving) is Hebrew-only; everything else keeps the old
    // PDF→DOCX behavior.
    if !target.eq_ignore_ascii_case("hebrew") {
        return pdf_to_docx_fallback(app, input, target).await;
    }

    let groq = crate::secrets::get_key(app, "groq");
    let cerebras = crate::secrets::get_key(app, "cerebras");
    if groq.is_none() && cerebras.is_none() {
        return Err(
            "לא הוגדרו מפתחות API. הוסף מפתח Groq או Cerebras בהגדרות → תרגום מסמכים.".into(),
        );
    }

    let exe = resolve_sidecar(app)?;
    let out = super::output_path_with_ext(input, target, "pdf");

    // RTL layout mode (same-box default) + batch concurrency. Unknown layout values
    // are tolerated by the sidecar, which falls back to same-box itself.
    let settings = crate::settings::load_or_init(app).ok();
    let rtl_layout = settings
        .as_ref()
        .map(|s| s.pdf_rtl_layout.clone())
        .unwrap_or_else(|| "same-box".into());
    // Translate batches in parallel only when the PRIMARY provider's key is paid
    // (Groq is tried first; Cerebras is primary only when there's no Groq key).
    // Free-tier stays at 1 to respect per-minute limits.
    let primary_paid = settings
        .as_ref()
        .map(|s| if groq.is_some() { s.groq_paid } else { s.cerebras_paid })
        .unwrap_or(false);
    let concurrency: u32 = if primary_paid { 6 } else { 1 };

    let app2 = app.clone();
    let exe2 = exe.clone();
    let in2 = input.to_path_buf();
    let out2 = out.clone();
    let (code, stderr, saved) = tokio::task::spawn_blocking(move || {
        run_sidecar(&app2, &exe2, &in2, &out2, groq, cerebras, &rtl_layout, concurrency)
    })
    .await
    .map_err(|e| e.to_string())??;

    // Prefer the path the sidecar actually wrote (may be a `_new.pdf` fallback if
    // the intended target was locked).
    if let Some(p) = saved {
        if Path::new(&p).exists() {
            return Ok(p);
        }
    }
    if out.exists() {
        return Ok(out.to_string_lossy().into_owned());
    }
    if code == 2 {
        return Err(if stderr.is_empty() {
            "לא נמצא טקסט לתרגום ב-PDF (ייתכן שזהו PDF סרוק/תמונה).".into()
        } else {
            stderr
        });
    }
    Err(if stderr.is_empty() {
        format!("תרגום ה-PDF נכשל (קוד {code}).")
    } else {
        stderr
    })
}

/// Runs the sidecar synchronously (call inside `spawn_blocking`). Streams its
/// stdout, re-emitting `PROGRESS d/t` lines as the `speakly://translate-progress`
/// event, and captures the final `SAVED <path>` line plus stderr. Returns
/// `(exit_code, stderr, saved_path)`.
fn run_sidecar(
    app: &AppHandle,
    exe: &Path,
    input: &Path,
    out: &Path,
    groq: Option<String>,
    cerebras: Option<String>,
    rtl_layout: &str,
    concurrency: u32,
) -> Result<(i32, String, Option<String>), String> {
    let mut cmd = Command::new(exe);
    cmd.arg(input)
        .arg("1-99999") // page range; the sidecar clamps to the real page count
        .arg("--out")
        .arg(out)
        .arg("--target")
        .arg("Hebrew")
        .arg("--rtl-layout")
        .arg(rtl_layout)
        .arg("--concurrency")
        .arg(concurrency.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Keys go through the environment only — never argv (would show in the process list).
    if let Some(g) = groq {
        cmd.env("GROQ_API_KEY", g);
    }
    if let Some(c) = cerebras {
        cmd.env("CEREBRAS_API_KEY", c);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW — no console flash
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("שגיאה בהפעלת מנוע ה-PDF: {e}"))?;

    let mut saved: Option<String> = None;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().flatten() {
            if let Some(rest) = line.strip_prefix("PROGRESS ") {
                if let Some((d, t)) = rest.split_once('/') {
                    if let (Ok(b), Ok(tot)) = (d.trim().parse::<u32>(), t.trim().parse::<u32>()) {
                        let _ = app.emit_to(
                            "mic",
                            "speakly://translate-progress",
                            serde_json::json!({ "batch": b, "total": tot }),
                        );
                    }
                }
            } else if let Some(p) = line.strip_prefix("SAVED ") {
                saved = Some(p.trim().to_string());
            }
        }
    }

    // stderr volume is tiny (errors only, emitted near exit) and stdout is drained
    // live above, so reading stderr after the stdout EOF cannot deadlock.
    let mut err_text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err_text);
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    Ok((status.code().unwrap_or(-1), err_text.trim().to_string(), saved))
}

/// Locates the bundled `timluli-pdf.exe`. With `"resources": ["resources/*"]`
/// Tauri places it under `<resource_dir>/resources/`; dev layouts vary, so a few
/// candidates are probed.
fn resolve_sidecar(app: &AppHandle) -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join("resources").join("timluli-pdf.exe"));
        candidates.push(dir.join("timluli-pdf.exe"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        // `tauri dev` runs with the cwd at src-tauri/.
        candidates.push(cwd.join("resources").join("timluli-pdf.exe"));
    }

    for p in &candidates {
        if p.exists() {
            return Ok(p.clone());
        }
    }
    Err(format!(
        "מנוע ה-PDF (timluli-pdf.exe) לא נמצא. ודא שהורץ src-tauri/sidecar/build_sidecar.ps1. נבדקו: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" ; ")
    ))
}

/// Legacy PDF→DOCX path: extracts text per page, splits into paragraphs,
/// translates, and writes a `.docx`. Used for non-Hebrew targets, where the RTL
/// PDF rendering does not apply.
async fn pdf_to_docx_fallback(
    app: &AppHandle,
    input: &Path,
    target: &str,
) -> Result<String, String> {
    let input_owned = input.to_path_buf();
    let pages = tokio::task::spawn_blocking(move || pdf_extract::extract_text_by_pages(&input_owned))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("שגיאה בקריאת PDF: {e:?}"))?;

    let mut paragraphs: Vec<String> = Vec::new();
    for page in &pages {
        paragraphs.extend(split_paragraphs(page));
    }

    if paragraphs.is_empty() {
        return Err("לא נמצא טקסט לתרגום ב-PDF (ייתכן שזהו PDF סרוק/תמונה).".into());
    }

    let units: Vec<(usize, String)> = paragraphs
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.clone()))
        .collect();

    let map = super::translate_units(app, target, super::SYSTEM_PROMPT_DOCUMENT, 5, &units).await?;

    let mut docx = Docx::default();
    for (i, original) in paragraphs.iter().enumerate() {
        let text = map.get(&i).cloned().unwrap_or_else(|| original.clone());
        let para = Paragraph::default()
            .property(ParagraphProperty {
                bidi: Some(Bidi { value: Some(true) }),
                ..Default::default()
            })
            .push(
                Run::default()
                    .property(CharacterProperty {
                        rtl: Some(RightToLeftText { value: Some(true) }),
                        ..Default::default()
                    })
                    .push_text(text),
            );
        docx.document.push(para);
    }

    let out = super::output_path_with_ext(input, target, "docx");
    docx.write_file(&out)
        .map_err(|e| format!("שגיאה בכתיבת DOCX: {e:?}"))?;
    Ok(out.to_string_lossy().into_owned())
}

/// Splits extracted page text into paragraphs on blank lines, trimming each.
fn split_paragraphs(text: &str) -> Vec<String> {
    let mut paras = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.trim().is_empty() {
                paras.push(cur.trim().to_string());
            }
            cur.clear();
        } else {
            if !cur.is_empty() {
                cur.push('\n');
            }
            cur.push_str(line);
        }
    }
    if !cur.trim().is_empty() {
        paras.push(cur.trim().to_string());
    }
    paras
}
