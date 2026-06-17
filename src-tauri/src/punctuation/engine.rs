//! In-process Hebrew punctuation engine: ONNX Runtime (via `ort`, load-dynamic) +
//! the HF `tokenizers` crate. Loads a quantized xlm-roberta punctuation model once
//! and restores `. , ?` on raw STT text. Validated end-to-end against the Python
//! reference (see Desktop\punct-gate\): ~26 ms/sentence on CPU.
//!
//! Decode = word-grouping: the original whitespace words are preserved verbatim and
//! a normalized mark is appended after each word, taken from the `post_preds` of the
//! word's last subtoken. We use only the `post_preds` head (ignore pre/cap/seg).

use std::path::Path;

use ort::session::Session;
use ort::value::Value;
use tokenizers::Tokenizer;

/// Index → punctuation mark for the model's `post_labels` (config.yaml order).
const POST_LABELS: &[&str] = &[
    "<NULL>", "<ACRONYM>", ".", ",", "?", "？", "，", "。", "、", "・", "।", "؟", "،", ";", "።", "፣",
    "፧",
];

const BOS: i64 = 0;
const EOS: i64 = 2;
const METASPACE: char = '\u{2581}'; // ▁ word-boundary marker

/// Normalize CJK/Arabic punctuation variants to the marks we want in Hebrew/Latin
/// text. `<ACRONYM>` (period after each char of an acronym) collapses to a period.
fn normalize_mark(m: &str) -> &str {
    match m {
        "？" => "?",
        "，" | "、" | "،" => ",",
        "。" | "।" => ".",
        "؟" | "፧" => "?",
        "<ACRONYM>" => ".",
        other => other,
    }
}

/// Hebrew interrogative openers. The model (Hebrew is an untrained transfer
/// language for it) reliably gets periods/commas but often misses question marks,
/// so when a segment opens with one of these and the model didn't end it with `?`,
/// we force a `?`. Kept deliberately small to avoid false positives.
// Hebrew interrogatives. The model (Hebrew is an untrained transfer language for it)
// nails periods/commas but misses question marks, so we detect questions ourselves.
// STRONG: almost always a real question opener. AMBIG: also used relatively
// ("מה שאמרת", "מי שבא") — only counted when NOT followed by a ש-word.
const STRONG_Q: &[&str] = &[
    "האם", "למה", "מדוע", "כיצד", "מתי", "איפה", "לאן", "מאיפה", "מניין", "היכן", "לכמה", "וכי",
];
const AMBIG_Q: &[&str] = &["מה", "מי", "כמה", "איך", "איזה", "איזו"];

/// Does a sentence look like a Hebrew question? Hebrew front-loads the interrogative,
/// so we scan the first few words (catching colloquial openers like "זה למה...").
/// A leading vav ("ו") conjunction is stripped.
fn is_question_sentence(words: &[&str]) -> bool {
    let scan = words.len().min(3);
    for i in 0..scan {
        let w = words[i].trim_matches(|c: char| !c.is_alphabetic());
        let bare = w.strip_prefix('ו').unwrap_or(w);
        if STRONG_Q.contains(&w) || STRONG_Q.contains(&bare) {
            return true;
        }
        if AMBIG_Q.contains(&w) || AMBIG_Q.contains(&bare) {
            // Relative use ("מה ש...", "מי ש...") is not a question.
            if !words.get(i + 1).copied().unwrap_or("").starts_with('ש') {
                return true;
            }
        }
    }
    false
}

/// Bare interrogative form: strip surrounding non-letters and a leading vav.
fn bare(w: &str) -> &str {
    let w = w.trim_matches(|c: char| !c.is_alphabetic());
    w.strip_prefix('ו').unwrap_or(w)
}

// Verbs/markers that make a FOLLOWING interrogative an INDIRECT question (a statement),
// so we must not split/`?` there: "אני לא יודע למה..." stays a statement.
const INDIRECT: &[&str] = &[
    "יודע", "יודעת", "יודעים", "יודעות", "לדעת", "מבין", "מבינה", "מבינים", "להבין", "זוכר",
    "זוכרת", "לזכור", "אמר", "אמרה", "אמרו", "אומר", "אומרת", "שאל", "שאלתי", "שואל", "לשאול",
    "מעניין", "תלוי", "בודק", "לבדוק", "ראיתי", "רואה", "הבנתי", "שכחתי", "בטוח", "בטוחה",
    "להסביר", "מסביר", "תסביר", "תגיד", "סיפר", "סיפרה", "להחליט", "החליט", "תוהה",
];
// Colloquial fillers that belong to the FOLLOWING question ("זה למה...", "אז מתי...").
const FILLER: &[&str] = &["זה", "זאת", "אז"];

fn is_indirect_prev(words: &[&str], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    let p = words[i - 1].trim_matches(|c: char| !c.is_alphabetic());
    INDIRECT.contains(&p)
        || ["יוד", "מבי", "זוכר", "אומר", "אמר"]
            .iter()
            .any(|s| p.starts_with(s))
}

/// Split a word list into clauses, starting a new clause before a STRONG interrogative
/// that opens a question mid-sentence (unless it's an indirect question). A trailing
/// filler ("זה"/"אז") is carried into the new question clause.
fn split_clauses<'a>(words: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut clauses: Vec<Vec<&str>> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 && !cur.is_empty() && STRONG_Q.contains(&bare(w)) && !is_indirect_prev(words, i) {
            let carry = (cur.len() > 1 && FILLER.contains(&bare(cur[cur.len() - 1])))
                .then(|| cur.pop().unwrap());
            clauses.push(std::mem::take(&mut cur));
            if let Some(c) = carry {
                cur.push(c);
            }
        }
        cur.push(w);
    }
    if !cur.is_empty() {
        clauses.push(cur);
    }
    clauses
}

/// Post-pass over the model's output: split on the model's terminal marks, further split
/// run-ons at mid-sentence interrogatives, mark question clauses with `?`, and (when
/// `ensure_terminal`) give the final mark-less clause a terminal mark.
fn apply_questions_and_terminal(text: &str, ensure_terminal: bool) -> String {
    // 1. Split on the model's terminal marks into sentences (commas stay in the body).
    let mut sentences: Vec<(String, Option<char>)> = Vec::new();
    let mut buf = String::new();
    for c in text.chars() {
        if c == '.' || c == '?' || c == '!' {
            sentences.push((std::mem::take(&mut buf), Some(c)));
        } else {
            buf.push(c);
        }
    }
    if !buf.trim().is_empty() {
        sentences.push((buf, None));
    } else if !buf.is_empty() {
        if let Some(last) = sentences.last_mut() {
            last.0.push_str(&buf);
        }
    }

    // 2. Flatten into clauses (splitting run-ons at mid-sentence interrogatives).
    struct Piece {
        body: String,
        question: bool,
        model_mark: Option<char>,
    }
    let mut pieces: Vec<Piece> = Vec::new();
    for (body, mark) in &sentences {
        let words: Vec<&str> = body.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let clauses = split_clauses(&words);
        let last_ci = clauses.len() - 1;
        for (ci, clause) in clauses.iter().enumerate() {
            pieces.push(Piece {
                body: clause.join(" "),
                question: is_question_sentence(clause),
                model_mark: if ci == last_ci { *mark } else { None },
            });
        }
    }

    // 3. Render, assigning each clause a terminal mark.
    let mut out = String::with_capacity(text.len() + 4);
    let n = pieces.len();
    for (idx, p) in pieces.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        let is_last = idx == n - 1;
        let mark = if p.question {
            Some('?')
        } else if let Some(m) = p.model_mark {
            Some(m)
        } else if is_last && ensure_terminal {
            Some('.')
        } else if is_last {
            None
        } else {
            Some('.')
        };
        // Strip a trailing comma/semicolon the model left before we add a terminal mark
        // (avoids "עולה,?").
        if mark.is_some() {
            out.push_str(p.body.trim_end_matches([',', '\u{060C}', ';', ' ']));
        } else {
            out.push_str(&p.body);
        }
        if let Some(m) = mark {
            out.push(m);
        }
    }
    out
}

pub struct PunctEngine {
    session: Session,
    tokenizer: Tokenizer,
}

impl PunctEngine {
    /// Loads the ONNX model + tokenizer. `onnxruntime.dll` is resolved next to the
    /// executable (bundled app-locally, like the vcruntime DLLs) and handed to `ort`
    /// once via load-dynamic.
    pub fn load(model_path: &Path, tokenizer_path: &Path) -> Result<Self, String> {
        crate::onnx_runtime::init()?;
        let session = Session::builder()
            .map_err(|e| format!("ort builder: {e}"))?
            .with_intra_threads(num_cpus::get().min(4))
            .map_err(|e| format!("ort threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("טעינת מודל הפיסוק נכשלה: {e}"))?;
        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| format!("טעינת מנתח הפיסוק נכשלה: {e}"))?;
        Ok(Self { session, tokenizer })
    }

    /// Restores punctuation. Returns the original text unchanged on any internal
    /// inconsistency (safe degradation — punctuation never breaks dictation).
    /// `ensure_terminal`: when true (a finalized utterance), guarantee the segment
    /// ends with a terminal mark — patches the model's weak sentence-final periods.
    pub fn punctuate(&mut self, text: &str, ensure_terminal: bool) -> String {
        // Preserve a trailing space (the online engine sends `text.trim() + ' '` so
        // consecutive injected segments stay separated; run() rebuilds from words and
        // drops it).
        let had_trailing_ws = text.ends_with(|c: char| c.is_whitespace());
        match self.run(text) {
            Ok(out) => {
                let mut out = apply_questions_and_terminal(&out, ensure_terminal);
                if had_trailing_ws && !out.is_empty() {
                    out.push(' ');
                }
                out
            }
            Err(_) => text.to_string(),
        }
    }

    fn run(&mut self, text: &str) -> Result<String, String> {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() {
            return Ok(text.to_string());
        }
        let enc = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| format!("encode: {e}"))?;
        let ids = enc.get_ids();
        let tokens = enc.get_tokens();
        if ids.is_empty() {
            return Ok(text.to_string());
        }

        // Group subtoken indices into words by the leading ▁ marker. A *lone* ▁ token
        // also starts a word (SentencePiece emits it as the start of some words, e.g.
        // "פגשתי" → ["▁","פג","שתי"]).
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut cur: Vec<usize> = Vec::new();
        for (k, t) in tokens.iter().enumerate() {
            if t.starts_with(METASPACE) && !cur.is_empty() {
                groups.push(std::mem::take(&mut cur));
            }
            cur.push(k);
        }
        if !cur.is_empty() {
            groups.push(cur);
        }
        // Drop groups that are ENTIRELY whitespace — a trailing lone "▁" the tokenizer
        // emits for a trailing space. CRITICAL: the online engine sends `text.trim() +
        // ' '`, so without this every online segment mis-aligns (groups = words + 1) and
        // falls back to raw text — the model output gets silently discarded.
        groups.retain(|g| {
            !g.iter()
                .all(|&k| tokens[k].trim_start_matches(METASPACE).trim().is_empty())
        });
        if groups.len() != words.len() {
            return Ok(text.to_string()); // alignment fallback
        }

        // input_ids = [BOS] + ids + [EOS]
        let mut input_ids: Vec<i64> = Vec::with_capacity(ids.len() + 2);
        input_ids.push(BOS);
        input_ids.extend(ids.iter().map(|&i| i as i64));
        input_ids.push(EOS);
        let n = input_ids.len();
        let input = Value::from_array(([1_usize, n], input_ids))
            .map_err(|e| format!("input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs!["input_ids" => input])
            .map_err(|e| format!("inference: {e}"))?;
        let (_shape, post) = outputs["post_preds"]
            .try_extract_tensor::<i64>()
            .map_err(|e| format!("extract: {e}"))?;

        // Rebuild the text with the model's marks. Question marks + sentence-final
        // terminals are applied afterward by `apply_questions_and_terminal`.
        let mut out = String::with_capacity(text.len() + 8);
        for (wi, grp) in groups.iter().enumerate() {
            let last = *grp.last().unwrap();
            let idx = post.get(last + 1).copied().unwrap_or(0) as usize; // +1 for BOS
            let label = POST_LABELS.get(idx).copied().unwrap_or("<NULL>");
            out.push_str(words[wi]);
            if label != "<NULL>" {
                out.push_str(normalize_mark(label));
            }
            out.push(' ');
        }
        Ok(out.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // End-to-end check of the real app code path against the validated INT8 model.
    // Needs the local artifacts + onnxruntime.dll next to the test exe — not in CI.
    // Run: cargo test --lib -- --ignored punctuates_hebrew --nocapture
    #[test]
    #[ignore]
    fn punctuates_hebrew() {
        let base = PathBuf::from(r"C:\Users\Lenovo\Desktop\punct-gate\out");
        let mut e = PunctEngine::load(&base.join("ml.int8.onnx"), &base.join("tokenizer.json"))
            .expect("load");
        let out = e.punctuate("שלום מה שלומך אני מקווה שהכל בסדר אצלך", true);
        println!("OUT: {out}");
        assert!(out.contains(','), "expected a comma: {out}");
        assert!(
            out.ends_with('.') || out.ends_with('?'),
            "expected terminal mark: {out}"
        );
        // wh-word opener heuristic should force a question mark somewhere.
        let q = e.punctuate("מה השעה עכשיו", true);
        println!("Q: {q}");
        assert!(q.ends_with('?'), "wh-opener should end with ?: {q}");

        // Regression: the online engine sends `text.trim() + ' '` (trailing space),
        // which made the Rust tokenizer emit an extra trailing ▁ → group/word mismatch
        // → raw text returned. Trailing space must still yield internal punctuation.
        let trailing = e.punctuate("שלום מה שלומך אני מקווה שהכל בסדר אצלך ", true);
        println!("TRAILING: {trailing:?}");
        assert!(
            trailing.contains(','),
            "trailing-space input must still be punctuated, got: {trailing:?}"
        );

        // Per-sentence Hebrew question heuristic (real user dictation + edge cases).
        for t in [
            "שלום לכם חברים חברות יש לי כמה שאלות לשאול אותכם לגבי כל המוצר הזה אז בואו נתחיל שאלה מספר אחת זה למה בכלל כדאי מוצר כזה שתיים האם זה שווה את הכסף",
            "למה בכלל כדאי מוצר כזה",
            "האם זה שווה את הכסף",
            "מה שאמרת נכון מאוד",
        ] {
            println!("CASE: {}", e.punctuate(t, true));
        }
        assert!(e.punctuate("למה בכלל כדאי מוצר כזה", true).ends_with('?'));
        assert!(e.punctuate("האם זה שווה את הכסף", true).ends_with('?'));
        // Relative "מה ש..." must NOT become a question.
        assert!(!e.punctuate("מה שאמרת נכון מאוד", true).contains('?'));
    }
}
