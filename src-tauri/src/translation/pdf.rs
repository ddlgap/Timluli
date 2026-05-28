//! PDF adapter: extracts text per page, splits into paragraphs, translates, and
//! writes the result as a `.docx` (rebuilding a laid-out PDF — especially RTL —
//! is unreliable, so DOCX output mirrors the reference Python behavior).

use std::path::Path;

use docx_rust::document::{Paragraph, Run};
use docx_rust::formatting::{Bidi, CharacterProperty, ParagraphProperty, RightToLeftText};
use docx_rust::Docx;
use tauri::AppHandle;

pub async fn translate_pdf(app: &AppHandle, input: &Path, target: &str) -> Result<String, String> {
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
