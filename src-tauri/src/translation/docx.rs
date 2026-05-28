//! DOCX adapter: translates body and table-cell paragraphs in place, preserving
//! document structure, then marks translated paragraphs/runs right-to-left so
//! Hebrew (and other RTL targets) render correctly in Word.

use std::collections::HashMap;
use std::path::Path;

use docx_rust::document::{Body, BodyContent, Paragraph, ParagraphContent, TableCellContent, TableRowContent};
use docx_rust::formatting::{Bidi, CharacterProperty, ParagraphProperty, RightToLeftText};
use docx_rust::DocxFile;
use tauri::AppHandle;

/// Translates `src` (a .docx) and writes `<stem>.<lang>.docx` next to `name_basis`.
/// `name_basis` differs from `src` only for legacy `.doc` (where `src` is a temp
/// conversion and the output should sit beside the original `.doc`).
pub async fn translate_docx(
    app: &AppHandle,
    src: &Path,
    name_basis: &Path,
    target: &str,
) -> Result<String, String> {
    // Pass 1: read paragraph texts in document order (no borrows held across await).
    let texts = {
        let file = DocxFile::from_file(src).map_err(|e| format!("שגיאה בפתיחת DOCX: {e:?}"))?;
        let docx = file.parse().map_err(|e| format!("שגיאה בפענוח DOCX: {e:?}"))?;
        paragraph_texts(&docx.document.body)
    };

    let units: Vec<(usize, String)> = texts
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(i, t)| (i, t.clone()))
        .collect();

    let out = super::output_path_with_ext(name_basis, target, "docx");

    let map = if units.is_empty() {
        HashMap::new()
    } else {
        super::translate_units(app, target, super::SYSTEM_PROMPT_DOCUMENT, 5, &units).await?
    };

    // Pass 2: re-parse, apply translations + RTL, write.
    let file = DocxFile::from_file(src).map_err(|e| format!("שגיאה בפתיחת DOCX: {e:?}"))?;
    let mut docx = file.parse().map_err(|e| format!("שגיאה בפענוח DOCX: {e:?}"))?;
    apply_translations(&mut docx.document.body, &map);
    docx.write_file(&out)
        .map_err(|e| format!("שגיאה בכתיבת DOCX: {e:?}"))?;

    Ok(out.to_string_lossy().into_owned())
}

/// Collects every paragraph's text in document order (body paragraphs and
/// table-cell paragraphs). Order must match `apply_translations`.
fn paragraph_texts(body: &Body) -> Vec<String> {
    let mut out = Vec::new();
    for content in &body.content {
        match content {
            BodyContent::Paragraph(p) => out.push(p.text()),
            BodyContent::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        if let TableRowContent::TableCell(tc) = cell {
                            for cc in &tc.content {
                                match cc {
                                    TableCellContent::Paragraph(p) => out.push(p.text()),
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Walks paragraphs in the same order as `paragraph_texts`, replacing the text of
/// any paragraph whose index has a translation.
fn apply_translations(body: &mut Body, map: &HashMap<usize, String>) {
    let mut idx = 0usize;
    for content in body.content.iter_mut() {
        match content {
            BodyContent::Paragraph(p) => {
                apply_one(p, idx, map);
                idx += 1;
            }
            BodyContent::Table(t) => {
                for row in t.rows.iter_mut() {
                    for cell in row.cells.iter_mut() {
                        if let TableRowContent::TableCell(tc) = cell {
                            for cc in tc.content.iter_mut() {
                                match cc {
                                    TableCellContent::Paragraph(p) => {
                                        apply_one(p, idx, map);
                                        idx += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn apply_one(p: &mut Paragraph<'_>, idx: usize, map: &HashMap<usize, String>) {
    let Some(new_text) = map.get(&idx) else {
        return;
    };

    // Collapse all run text into the first text node (mirrors python-docx).
    let mut first = true;
    for cow in p.iter_text_mut() {
        if first {
            *cow = new_text.clone().into();
            first = false;
        } else {
            *cow = "".into();
        }
    }

    // RTL paragraph layout.
    if p.property.is_none() {
        p.property = Some(ParagraphProperty::default());
    }
    if let Some(pp) = p.property.as_mut() {
        pp.bidi = Some(Bidi { value: Some(true) });
    }

    // RTL on each run.
    for c in p.content.iter_mut() {
        if let ParagraphContent::Run(r) = c {
            if r.property.is_none() {
                r.property = Some(CharacterProperty::default());
            }
            if let Some(cp) = r.property.as_mut() {
                cp.rtl = Some(RightToLeftText { value: Some(true) });
            }
        }
    }
}
