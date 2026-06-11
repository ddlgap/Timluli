//! Format adapters ported from the reference Python translator
//! (cerabras/translate_drop.py). Each format parses into translatable chunks
//! that preserve structure (subtitle indices/timings, paragraph separators) in
//! `Meta`, sends only the spoken/prose text to the model, and re-renders the
//! original structure around the translated text.

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Srt,
    Vtt,
    Sbv,
    Txt,
    Md,
}

pub enum Category {
    Subtitle,
    Document,
}

impl Format {
    pub fn from_ext(ext: &str) -> Option<Format> {
        match ext {
            "srt" => Some(Format::Srt),
            "vtt" => Some(Format::Vtt),
            "sbv" => Some(Format::Sbv),
            "txt" => Some(Format::Txt),
            "md" | "markdown" => Some(Format::Md),
            _ => None,
        }
    }

    pub fn category(self) -> Category {
        match self {
            Format::Srt | Format::Vtt | Format::Sbv => Category::Subtitle,
            Format::Txt | Format::Md => Category::Document,
        }
    }

    pub fn batch_size(self) -> usize {
        match self {
            Format::Srt | Format::Vtt | Format::Sbv => 20,
            Format::Txt => 6,
            Format::Md => 5,
        }
    }
}

enum Meta {
    Srt { timing: String },
    Vtt { cue_name: Option<String>, timing: String },
    Sbv { timing: String },
    Txt { sep_after: String },
    Md,
}

pub struct Chunk {
    pub id: usize,
    pub text: String,
    pub translatable: bool,
    meta: Meta,
}

impl Chunk {
    /// The cue's time window in milliseconds, for subtitle chunks whose timing
    /// parses cleanly (`None` for documents or malformed timings). Used to match
    /// cues against the time-keyed `<stem>.genders.json` sidecar.
    pub fn time_ms(&self) -> Option<(u64, u64)> {
        match &self.meta {
            Meta::Srt { timing } | Meta::Vtt { timing, .. } => {
                let (a, b) = timing.split_once("-->")?;
                // VTT may append cue settings after the end time, e.g.
                // "00:00:01.000 --> 00:00:02.000 line:0" — keep the first token.
                let end = b.split_whitespace().next()?;
                Some((parse_ts_ms(a.trim())?, parse_ts_ms(end)?))
            }
            Meta::Sbv { timing } => {
                let (a, b) = timing.trim().split_once(',')?;
                Some((parse_ts_ms(a.trim())?, parse_ts_ms(b.trim())?))
            }
            _ => None,
        }
    }
}

/// Parses `HH:MM:SS,mmm` (SRT), `HH:MM:SS.mmm` / `MM:SS.mmm` (VTT), or
/// `H:MM:SS.mmm` (SBV) into milliseconds.
fn parse_ts_ms(ts: &str) -> Option<u64> {
    let (hms, millis) = ts
        .split_once(',')
        .or_else(|| ts.split_once('.'))
        .unwrap_or((ts, "0"));
    let millis: u64 = millis.trim().parse().ok()?;
    let parts: Vec<u64> = hms
        .split(':')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [h, m, s] => h * 3600 + m * 60 + s,
        [m, s] => m * 60 + s,
        _ => return None,
    };
    Some(secs * 1000 + millis)
}

pub struct ParsedDoc {
    format: Format,
    pub chunks: Vec<Chunk>,
    vtt_header: String,
}

impl ParsedDoc {
    pub fn category(&self) -> Category {
        self.format.category()
    }
    pub fn batch_size(&self) -> usize {
        self.format.batch_size()
    }

    /// Re-renders the document, substituting translated text by chunk id and
    /// keeping the original text for any id missing from `translated`.
    pub fn render(&self, translated: &HashMap<usize, String>) -> String {
        let text_of = |c: &Chunk| -> String {
            translated.get(&c.id).cloned().unwrap_or_else(|| c.text.clone())
        };
        match self.format {
            Format::Srt => {
                let blocks: Vec<String> = self
                    .chunks
                    .iter()
                    .map(|c| match &c.meta {
                        Meta::Srt { timing } => format!("{}\n{}\n{}\n", c.id, timing, text_of(c)),
                        _ => String::new(),
                    })
                    .collect();
                blocks.join("\n")
            }
            Format::Sbv => {
                let blocks: Vec<String> = self
                    .chunks
                    .iter()
                    .map(|c| match &c.meta {
                        Meta::Sbv { timing } => format!("{}\n{}", timing, text_of(c)),
                        _ => String::new(),
                    })
                    .collect();
                blocks.join("\n\n") + "\n"
            }
            Format::Vtt => {
                let mut parts: Vec<String> = vec![self.vtt_header.clone()];
                for c in &self.chunks {
                    if let Meta::Vtt { cue_name, timing } = &c.meta {
                        let name = cue_name
                            .as_ref()
                            .map(|n| format!("{n}\n"))
                            .unwrap_or_default();
                        parts.push(format!("{name}{timing}\n{}\n", text_of(c)));
                    }
                }
                parts.join("\n").trim_end().to_string() + "\n"
            }
            Format::Txt => {
                let mut out = String::new();
                for c in &self.chunks {
                    out.push_str(&text_of(c));
                    if let Meta::Txt { sep_after } = &c.meta {
                        out.push_str(sep_after);
                    }
                }
                out
            }
            Format::Md => self.chunks.iter().map(text_of).collect(),
        }
    }
}

pub fn parse(format: Format, content: &str) -> ParsedDoc {
    let (chunks, vtt_header) = match format {
        Format::Srt => (parse_srt(content), String::new()),
        Format::Sbv => (parse_sbv(content), String::new()),
        Format::Vtt => parse_vtt(content),
        Format::Txt => (parse_txt(content), String::new()),
        Format::Md => (parse_md(content), String::new()),
    };
    ParsedDoc {
        format,
        chunks,
        vtt_header,
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Groups consecutive non-blank lines into blocks (blank line = separator).
fn split_blocks(content: &str) -> Vec<Vec<&str>> {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                blocks.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(line);
        }
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    blocks
}

/// Splits into lines keeping each line's trailing '\n' so concatenation is
/// lossless (used by TXT/MD which must round-trip exact whitespace).
fn split_keep_ends(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in content.chars() {
        cur.push(ch);
        if ch == '\n' {
            lines.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

// ─── SRT ─────────────────────────────────────────────────────────────────────

fn parse_srt(content: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for block in split_blocks(content) {
        if block.len() < 2 {
            continue;
        }
        let idx: usize = match block[0].trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if !block[1].contains("-->") {
            continue;
        }
        let timing = block[1].trim().to_string();
        let text = block[2..].join("\n");
        chunks.push(Chunk {
            id: idx,
            text,
            translatable: true,
            meta: Meta::Srt { timing },
        });
    }
    chunks
}

// ─── SBV ─────────────────────────────────────────────────────────────────────

fn parse_sbv(content: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for (i, block) in split_blocks(content).into_iter().enumerate() {
        let timing = block[0].to_string();
        let text = block[1..].join("\n");
        chunks.push(Chunk {
            id: i + 1,
            text,
            translatable: true,
            meta: Meta::Sbv { timing },
        });
    }
    chunks
}

// ─── VTT ─────────────────────────────────────────────────────────────────────

fn parse_vtt(content: &str) -> (Vec<Chunk>, String) {
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    let mut i = 0;

    let mut header_lines: Vec<&str> = Vec::new();
    while i < n && !lines[i].contains("-->") {
        header_lines.push(lines[i]);
        i += 1;
    }
    // Walk back to drop an optional cue identifier that precedes the first cue.
    while !header_lines.is_empty()
        && !header_lines.last().unwrap().trim().is_empty()
        && !header_lines.last().unwrap().starts_with("WEBVTT")
    {
        i -= 1;
        header_lines.pop();
    }
    let header = format!("{}\n\n", header_lines.join("\n").trim_end());

    let mut chunks = Vec::new();
    let mut cue_id = 0usize;
    while i < n {
        let mut cue_name: Option<String> = None;
        if !lines[i].contains("-->") {
            if !lines[i].trim().is_empty() {
                cue_name = Some(lines[i].trim().to_string());
            }
            i += 1;
            if i >= n {
                break;
            }
        }
        if i >= n || !lines[i].contains("-->") {
            continue;
        }
        let timing = lines[i].trim_end().to_string();
        i += 1;
        let mut text_lines: Vec<&str> = Vec::new();
        while i < n && !lines[i].trim().is_empty() {
            text_lines.push(lines[i]);
            i += 1;
        }
        cue_id += 1;
        chunks.push(Chunk {
            id: cue_id,
            text: text_lines.join("\n"),
            translatable: true,
            meta: Meta::Vtt { cue_name, timing },
        });
        while i < n && lines[i].trim().is_empty() {
            i += 1;
        }
    }
    (chunks, header)
}

// ─── TXT ─────────────────────────────────────────────────────────────────────

fn parse_txt(content: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut cid = 0usize;
    let mut para = String::new();
    let mut sep = String::new();
    let mut in_sep = false;

    fn flush(para: &mut String, sep: &mut String, cid: &mut usize, chunks: &mut Vec<Chunk>) {
        *cid += 1;
        let translatable = !para.trim().is_empty();
        chunks.push(Chunk {
            id: *cid,
            translatable,
            text: std::mem::take(para),
            meta: Meta::Txt {
                sep_after: std::mem::take(sep),
            },
        });
    }

    for line in split_keep_ends(content) {
        if line.trim().is_empty() {
            in_sep = true;
            sep.push_str(&line);
        } else {
            if in_sep {
                flush(&mut para, &mut sep, &mut cid, &mut chunks);
                in_sep = false;
            }
            para.push_str(&line);
        }
    }
    if !para.is_empty() || !sep.is_empty() {
        flush(&mut para, &mut sep, &mut cid, &mut chunks);
    }
    chunks
}

// ─── Markdown ──────────────────────────────────────────────────────────────────

fn parse_md(content: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut cid = 0usize;
    let mut buf = String::new();
    let mut in_code = false;

    fn flush(buf: &mut String, cid: &mut usize, chunks: &mut Vec<Chunk>, translatable: bool) {
        if buf.is_empty() {
            return;
        }
        *cid += 1;
        let text = std::mem::take(buf);
        let tr = translatable && !text.trim().is_empty();
        chunks.push(Chunk {
            id: *cid,
            translatable: tr,
            text,
            meta: Meta::Md,
        });
    }

    for line in split_keep_ends(content) {
        let is_fence = line.starts_with("```") || line.starts_with("~~~");
        if is_fence {
            if in_code {
                buf.push_str(&line);
                flush(&mut buf, &mut cid, &mut chunks, false);
                in_code = false;
            } else {
                flush(&mut buf, &mut cid, &mut chunks, true);
                buf.push_str(&line);
                in_code = true;
            }
            continue;
        }
        if in_code {
            buf.push_str(&line);
            continue;
        }
        if line.trim().is_empty() {
            buf.push_str(&line);
            flush(&mut buf, &mut cid, &mut chunks, true);
        } else {
            buf.push_str(&line);
        }
    }
    flush(&mut buf, &mut cid, &mut chunks, !in_code);
    chunks
}
