"""
Timluli PDF→PDF translator sidecar (layout-preserving, Hebrew RTL).

Packaged with PyInstaller as timluli-pdf.exe and launched from Rust
(translation/pdf.rs) exactly like the Chrome / LibreOffice sidecars. Cloud
translation goes through OpenAI-compatible chat-completion endpoints
(Groq / Cerebras) with the provider fallback chain mirroring
translation/mod.rs. All PDF surgery (block classification, redaction, RTL
rendering with PyMuPDF + python-bidi, figure capture) is unchanged from the
proven spike, so layout fidelity is preserved.

Host contract:
  argv:   <pdf_path> [page_range] --out <output_pdf> --target <language>
  stdin:  unused
  stdout: progress as "PROGRESS done/total" lines; final "SAVED <path>" line
  stderr: Hebrew error message on failure
  exit:   0 = success, 2 = no translatable text (scanned/image PDF), 1 = error

API keys come from environment variables only: GROQ_API_KEY and/or CEREBRAS_API_KEY
(never passed on the command line, so they never appear in the process list).
"""

import sys
import os
import re
import json
import time
import requests
import fitz  # PyMuPDF
from bidi.algorithm import get_display
from concurrent.futures import ThreadPoolExecutor, as_completed
import threading

def _reconfig_utf8(stream):
    """Force UTF-8 on a std stream if present (paths/text may contain Hebrew).

    Guarded: when frozen as a windowed exe and launched without redirected
    stdio, sys.stdout/stderr can be None. Timluli always pipes both, but stay safe.
    """
    try:
        if stream is not None:
            stream.reconfigure(encoding="utf-8")
    except Exception:
        pass


_reconfig_utf8(sys.stdout)
_reconfig_utf8(sys.stderr)


_print_lock = threading.Lock()

# ── Document-level progress (parsed by Timluli's Rust host) ─────────────────
# Rust reads stdout lines prefixed "PROGRESS done/total" and re-emits them as the
# speakly://translate-progress IPC event. `_g_total` is an upper bound computed up
# front (qualifying text blocks, before paragraph dedup / figure-region skipping),
# so `done` never exceeds it.
_progress_lock = threading.Lock()
_g_done = 0
_g_total = 0


def _emit_progress():
    """Increment the global done-counter and print one machine-readable line."""
    global _g_done
    with _progress_lock:
        _g_done += 1
        done = min(_g_done, _g_total) if _g_total else _g_done
    total = _g_total if _g_total else done
    print(f"PROGRESS {done}/{total}", flush=True)

# ── Cloud translation config (replaces Ollama) ──────────────────────────────
# OpenAI-compatible chat-completions endpoints.
PROVIDER_BASE = {
    "groq": "https://api.groq.com/openai/v1",
    "cerebras": "https://api.cerebras.ai/v1",
}

# Fallback chain mirrored from Timluli src-tauri/src/translation/mod.rs FALLBACK_CHAIN.
FALLBACK_CHAIN = [
    ("groq", "llama-3.3-70b-versatile"),
    ("groq", "openai/gpt-oss-120b"),
    ("groq", "llama-3.1-8b-instant"),
    ("cerebras", "qwen-3-235b-a22b-instruct-2507"),
    ("cerebras", "gpt-oss-120b"),
    ("cerebras", "llama3.1-8b"),
]

API_KEYS = {
    "groq": os.environ.get("GROQ_API_KEY", "").strip(),
    "cerebras": os.environ.get("CEREBRAS_API_KEY", "").strip(),
}

# Same full-fidelity prompt as the reference, parameterized by target language.
# The equation-reference rules (Figure->איור, Equation->משוואה) are Hebrew-specific;
# Timluli only routes Hebrew targets to this PDF->PDF path (see translation/pdf.rs).
def build_system_prompt(target: str) -> str:
    return (
        f"You are a professional English-to-{target} translator for physics textbooks. "
        f"Translate the given English text to {target}. "
        f"Output ONLY the {target} translation, nothing else. "
        "Do not explain, do not add notes, do not repeat the original. "
        "CRITICAL RULES:\n"
        "1. Keep ALL mathematical expressions EXACTLY as they appear. "
        "This includes: equations (F=ma, E=mc², v²/c²), variable names "
        "(c, v, q, S, S', F', λ, μ₀, ε₀, π), numbers with units "
        "(3.00 × 10⁸ m/s, 30 km/s), and formulas (kqλ/y₁, -μ₀λv²q/(2πy₁)).\n"
        "2. Do NOT translate or modify any part of an equation.\n"
        "3. Equations should appear in their original form embedded in the Hebrew text.\n"
        "4. Figure references like 'Figure 1-4' should become 'איור 1-4'.\n"
        "5. Equation references like 'Equation 1-3' should become 'משוואה 1-3'."
    )


# Default; main() overrides this from --target before any translation begins.
SYSTEM_PROMPT = build_system_prompt("Hebrew")

# Models that returned a permanent error this run (quota / 404 / auth) — skip thereafter.
_exhausted_models = set()
_exhausted_lock = threading.Lock()

# Hebrew-capable font on Windows
HEBREW_FONT = "C:/Windows/Fonts/david.ttf"
HEBREW_FONT_BOLD = "C:/Windows/Fonts/davidbd.ttf"
# Font names that indicate equation/math content — never translate these
MATH_FONTS = {"PearsonMATH", "MathematicalPi"}

# Map of characters that math fonts encode as "9" (prime) or other misleading glyphs.
# Applied to extracted text before translation and to translated output before rendering.
_PRIME_CLEANUP = str.maketrans({
    "⁹": "'",   # superscript nine -> prime
    "′": "'",   # prime symbol -> apostrophe
    "″": "''",  # double prime -> two apostrophes
    "ʹ": "'",   # modifier letter prime
    "ˊ": "'",   # modifier letter acute
})


def _fix_primes(text: str) -> str:
    """Replace math-font '9' primes and Unicode superscript primes with apostrophe.

    In many physics PDFs, prime marks (S', F', a') are encoded as '9' in math fonts.
    This function converts patterns like 'S9' -> "S'" and cleans Unicode superscripts.
    """
    # Fix Unicode superscript/prime characters
    text = text.translate(_PRIME_CLEANUP)
    # Fix math-font "9" primes: single uppercase/lowercase letter followed by 9
    # but NOT standalone numbers like "109" or "1-3"
    text = re.sub(r'(?<=[A-Za-z])9(?=[^0-9]|$)', "'", text)
    return text


# CID font -> Unicode character mappings.
# Math fonts use custom encodings where ASCII codes map to Greek/math glyphs.
# These mappings convert the garbled extracted text to proper Unicode.
_MATH_PI_ONE_MAP = str.maketrans({
    'l': 'λ', 'm': 'μ', 'p': 'π', 'e': 'ε',
    '2': '²',
})
_PEARSON_18_MAP = str.maketrans({'>': '/'})
_PEARSON_02_MAP = str.maketrans({'*': '×'})
_PEARSON_20_MAP = str.maketrans({
    '0': '₀', '1': '₁', '2': '₂', '3': '₃', '4': '₄',
    '5': '₅', '6': '₆', '7': '₇', '8': '₈', '9': '₉',
})


def _fix_math_text(text: str, font_name: str) -> str:
    """Map CID-garbled math font characters to proper Unicode.

    Math fonts like MathematicalPi encode Greek letters as ASCII (l->lambda, m->mu).
    PearsonMATH variants encode operators and subscripts differently.
    """
    text = _fix_primes(text)
    if 'MathematicalPi-One' in font_name:
        text = text.translate(_MATH_PI_ONE_MAP)
    elif 'MathematicalPi-Three' in font_name:
        text = text.replace('', '≈')
    elif 'PearsonMATH18' in font_name:
        text = text.translate(_PEARSON_18_MAP)
    elif 'PearsonMATH02' in font_name:
        text = text.translate(_PEARSON_02_MAP)
    elif 'PearsonMATH20' in font_name:
        text = text.translate(_PEARSON_20_MAP)
    # PearsonMATH08/12: = stays as = (correct)
    # Remove garbled private-use / replacement characters
    text = text.replace('�', '')
    text = text.replace('฀', 'ε')
    text = text.replace('', '')
    return text


def _chat_once(provider: str, model: str, key: str, text: str):
    """One OpenAI-compatible chat completion.

    Returns (content, None) on success, or (None, (kind, retry_after, msg)) on error,
    where kind is one of 'rate_limit' | 'quota' | 'transient'.
    """
    url = PROVIDER_BASE[provider] + "/chat/completions"
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": text},
        ],
        "temperature": 0.1,
        # Keep under free-tier TPM limits, like Timluli (max_completion_tokens=4096).
        "max_tokens": min(4096, max(512, len(text) * 3)),
        "stream": False,
    }
    try:
        resp = requests.post(
            url,
            json=body,
            headers={"Authorization": f"Bearer {key}"},
            timeout=120,
        )
    except Exception as e:
        return None, ("transient", None, str(e))

    sc = resp.status_code
    if sc == 429:
        ra = resp.headers.get("Retry-After")
        try:
            ra = int(float(ra)) if ra else None
        except (ValueError, TypeError):
            ra = None
        return None, ("rate_limit", ra, "429 rate limit")
    if sc == 402:
        return None, ("quota", None, "402 payment required / quota")
    if sc in (401, 403):
        return None, ("quota", None, f"{sc} auth/forbidden")
    if sc == 404:
        return None, ("quota", None, "404 model not found")
    if sc >= 400:
        # 413 / 5xx / other -> try next model for this unit.
        return None, ("transient", None, f"HTTP {sc}: {resp.text[:200]}")

    try:
        content = resp.json()["choices"][0]["message"]["content"].strip()
    except Exception as e:
        return None, ("transient", None, f"bad response json: {e}")
    return content, None


def translate_text(text: str) -> str:
    """Translate English text to Hebrew via Groq/Cerebras (OpenAI-compatible).

    Provider fallback chain + 429 retry (same model, up to 3x) + quota skip,
    mirroring Timluli's translate_units(). Returns the original text on total failure.
    """
    text = text.strip()
    if not text or len(text) < 2:
        return text
    # Skip if it's just numbers/symbols
    if not any(c.isalpha() for c in text):
        return text

    # Clean up math-font prime encoding (S9 -> S') before translation
    text = _fix_primes(text)

    last_err = None
    for provider, model in FALLBACK_CHAIN:
        label = f"{provider}:{model}"
        with _exhausted_lock:
            if label in _exhausted_models:
                continue
        key = API_KEYS.get(provider, "")
        if not key:
            with _exhausted_lock:
                _exhausted_models.add(label)
            continue

        rate_retries = 0
        while True:
            content, err = _chat_once(provider, model, key, text)
            if err is None:
                result = content.strip('"\'`')
                # Validate: result must contain Hebrew characters (U+0590..U+05FF)
                if not any("֐" <= c <= "׿" for c in result):
                    _tprint(f"  [Warning: no Hebrew from {label}, trying next model]")
                    last_err = f"{label}: no hebrew in response"
                    break  # -> next model
                # Clean up any superscript/prime characters the model may output
                result = _fix_primes(result)
                return result if result else text

            kind, retry_after, msg = err
            last_err = f"{label}: {msg}"
            if kind == "rate_limit":
                if rate_retries < 3:
                    rate_retries += 1
                    wait = retry_after if retry_after else 12
                    wait = max(2, min(30, wait))
                    _tprint(f"  [Rate limit on {label}, waiting {wait}s "
                            f"(retry {rate_retries}/3)]")
                    time.sleep(wait)
                    continue  # retry SAME model
                with _exhausted_lock:
                    _exhausted_models.add(label)
                break  # -> next model
            elif kind == "quota":
                with _exhausted_lock:
                    _exhausted_models.add(label)
                break  # -> next model
            else:  # transient
                break  # -> next model

    _tprint(f"  [Translation failed, keeping original. last error: {last_err}]")
    return text


def _block_has_math_font(block):
    """Check if a text block contains math/equation font spans."""
    for line in block["lines"]:
        for span in line["spans"]:
            if any(mf in span["font"] for mf in MATH_FONTS):
                return True
    return False


def _block_alpha_count(block):
    """Count alphabetic chars in non-math-font spans of a block."""
    count = 0
    for line in block["lines"]:
        for span in line["spans"]:
            if not any(mf in span["font"] for mf in MATH_FONTS):
                count += sum(1 for c in span["text"] if c.isalpha())
    return count


def _block_full_text(block):
    """Get all text in a block concatenated."""
    parts = []
    for line in block["lines"]:
        for span in line["spans"]:
            if span["text"].strip():
                parts.append(span["text"].strip())
    return " ".join(parts)


def wrap_hebrew_text(text, font, fontsize, max_width):
    """Word-wrap Hebrew text (logical order) into lines fitting max_width."""
    words = text.split()
    if not words:
        return [text]

    lines = []
    current = words[0]

    for word in words[1:]:
        test = current + " " + word
        if font.text_length(test, fontsize=fontsize) <= max_width:
            current = test
        else:
            lines.append(current)
            current = word
    lines.append(current)
    return lines


def _tprint(*args, **kwargs):
    """Thread-safe print."""
    with _print_lock:
        print(*args, **kwargs)


def translate_page(page: fitz.Page, page_num: int, source_page: fitz.Page = None):
    """Translate text on a page from English to Hebrew, preserving equations and layout.

    Paragraph-level approach:
    - Equation blocks (math fonts, <15 alpha) -> skip entirely
    - Short labels (<=3 chars) -> skip entirely
    - Body text blocks -> merge lines into paragraph, translate, render Hebrew
    - Mixed blocks (math fonts + >=15 alpha) -> translate text portions
    - Figures (vector drawing clusters) -> capture as images from source
    """
    _tprint(f"Processing page {page_num}...")

    text_dict = page.get_text("dict", flags=fitz.TEXT_PRESERVE_WHITESPACE)
    blocks = text_dict.get("blocks", [])

    # Detect figure regions (clusters of vector drawings) — capture as images
    figure_rects = []
    capture_page = source_page if source_page is not None else page
    capture_scale = 288.0 / 72.0  # 4x for crisp capture

    try:
        drawings = capture_page.get_drawings()
    except Exception:
        drawings = []

    figure_images = []  # (rect, png_bytes) for figure regions

    if drawings:
        page_w, page_h = page.rect.width, page.rect.height
        filtered = [d for d in drawings
                    if not (fitz.Rect(d["rect"]).width > page_w * 0.6 or
                            fitz.Rect(d["rect"]).height > page_h * 0.6 or
                            fitz.Rect(d["rect"]).width > 150 or
                            fitz.Rect(d["rect"]).height > 100 or
                            (fitz.Rect(d["rect"]).width < 0.5 and fitz.Rect(d["rect"]).height < 0.5))]

        used = [False] * len(filtered)
        clusters = []
        for i, d in enumerate(filtered):
            if used[i]:
                continue
            cluster = [d]; used[i] = True; stack = [i]
            while stack:
                ci = stack.pop()
                cr = fitz.Rect(filtered[ci]["rect"])
                for j, d2 in enumerate(filtered):
                    if used[j]:
                        continue
                    r2 = fitz.Rect(d2["rect"])
                    if (cr.x0 - 20 <= r2.x1 and r2.x0 <= cr.x1 + 20 and
                            cr.y0 - 20 <= r2.y1 and r2.y0 <= cr.y1 + 20):
                        cluster.append(d2); used[j] = True; stack.append(j)
            clusters.append(cluster)

        for cluster in clusters:
            if len(cluster) < 5:
                continue
            cx0 = min(d["rect"][0] for d in cluster)
            cy0 = min(d["rect"][1] for d in cluster)
            cx1 = max(d["rect"][2] for d in cluster)
            cy1 = max(d["rect"][3] for d in cluster)
            fig_rect = fitz.Rect(cx0, cy0, cx1, cy1)
            if fig_rect.width < 50 or fig_rect.height < 50:
                continue
            expanded = fitz.Rect(fig_rect)
            for block in blocks:
                if block["type"] != 0:
                    continue
                br = fitz.Rect(block["bbox"])
                padded = fitz.Rect(fig_rect.x0-10, fig_rect.y0-10, fig_rect.x1+10, fig_rect.y1+10)
                if br.intersects(padded) and len(_block_full_text(block)) < 30:
                    expanded |= br
            expanded = fitz.Rect(expanded.x0-5, expanded.y0-5, expanded.x1+5, expanded.y1+5)
            if any(expanded.intersects(fr) for fr in figure_rects):
                continue
            mat = fitz.Matrix(capture_scale, capture_scale)
            pix = capture_page.get_pixmap(matrix=mat, clip=expanded, alpha=False)
            figure_images.append((expanded, pix.tobytes("png")))
            figure_rects.append(expanded)

    # Phase 1: Classify blocks and collect translatable units
    units = []

    for block in blocks:
        if block["type"] != 0:
            continue
        block_rect = fitz.Rect(block["bbox"])
        # Skip blocks inside figure regions
        if any(block_rect.intersects(fr) for fr in figure_rects):
            continue
        full_text = _block_full_text(block)
        if not full_text or not any(c.isalpha() for c in full_text):
            continue
        # Skip short labels (axis labels like y, x, S, v)
        if len(full_text.replace(" ", "")) <= 3:
            continue
        # Skip pure equation blocks (math fonts + few alpha chars)
        if _block_has_math_font(block) and _block_alpha_count(block) < 15:
            continue

        # This block should be translated — merge lines into paragraph
        lines = block["lines"]
        line_texts = []
        all_spans = []

        for line in lines:
            parts = []
            for span in line["spans"]:
                t = span["text"]
                if t.strip():
                    # ALL spans get added to redact list (including math-font)
                    # to prevent black residue from un-redacted math characters
                    all_spans.append(span)

                    # Math-font spans -> fix CID encoding to proper Unicode
                    # (l->lambda, m->mu, >->/, etc.) then include in paragraph
                    if any(mf in span["font"] for mf in MATH_FONTS):
                        fixed = _fix_math_text(t.strip(), span["font"])
                        if fixed:
                            parts.append(fixed)
                        else:
                            if not parts or parts[-1] != " ":
                                parts.append(" ")
                        continue
                    parts.append(t)
            line_text = "".join(parts).strip()
            if line_text:
                line_texts.append(line_text)

        if not line_texts:
            continue

        # Merge lines into paragraph with hyphenation handling
        paragraph = ""
        for lt in line_texts:
            if paragraph.endswith("-"):
                # Check if this is a hyphenated word break (not a compound like "al-Haytham")
                paragraph = paragraph[:-1] + lt
            elif paragraph:
                paragraph += " " + lt
            else:
                paragraph = lt

        # Get block metrics from first non-math span
        first_span = all_spans[0] if all_spans else lines[0]["spans"][0]

        units.append({
            "paragraph": paragraph.strip(),
            "spans": all_spans,  # spans to redact
            "lines": lines,
            "font_size": first_span["size"],
            "color": first_span.get("color", 0),
            "is_bold": bool(first_span.get("flags", 0) & (2**4)),
            "block_x0": block["bbox"][0],
            "block_x1": block["bbox"][2],
            "block_y0": block["bbox"][1],
            "block_y1": block["bbox"][3],
        })

    if not units:
        _tprint(f"  No translatable text on page {page_num}.")
        for rect, png_bytes in figure_images:
            page.draw_rect(rect, color=None, fill=(1, 1, 1))
            page.insert_image(rect, stream=png_bytes, keep_proportion=True)
        return

    # Phase 2: Translate all paragraphs
    translations = {}
    for i, unit in enumerate(units):
        para = unit["paragraph"]
        if para and para not in translations:
            _emit_progress()
            _tprint(f"  [p{page_num}] Translating ({i+1}/{len(units)}): "
                  f"{para[:60]}{'...' if len(para)>60 else ''}")
            translations[para] = translate_text(para)

    # Phase 3: Redact original text spans (white fill)
    for unit in units:
        for span in unit["spans"]:
            rect = fitz.Rect(span["bbox"])
            page.add_redact_annot(rect, text="", fill=(1, 1, 1))
    page.apply_redactions(images=fitz.PDF_REDACT_IMAGE_NONE)

    # Phase 4: Render Hebrew translations
    MIN_FONT_SIZE = 6.0

    for unit in units:
        translated = translations.get(unit["paragraph"], unit["paragraph"])
        if not translated or not any("֐" <= ch <= "׿" for ch in translated):
            continue

        font_size = unit["font_size"]
        is_bold = unit["is_bold"]
        block_x0 = unit["block_x0"]
        block_x1 = unit["block_x1"]
        block_width = block_x1 - block_x0

        c_int = unit["color"]
        r_c = ((c_int >> 16) & 0xFF) / 255.0
        g_c = ((c_int >> 8) & 0xFF) / 255.0
        b_c = (c_int & 0xFF) / 255.0

        font_path = HEBREW_FONT_BOLD if is_bold else HEBREW_FONT
        if not os.path.exists(font_path):
            font_path = HEBREW_FONT
        heb_font = fitz.Font(fontfile=font_path)

        # Get line y-positions from original block
        lines = unit["lines"]
        y_tops = [line["bbox"][1] for line in lines]
        y_bottoms = [line["bbox"][3] for line in lines]

        # De-duplicate overlapping y-positions
        deduped_idx = [0]
        for k in range(1, len(y_tops)):
            if abs(y_tops[k] - y_tops[deduped_idx[-1]]) > 5:
                deduped_idx.append(k)
        y_tops = [y_tops[k] for k in deduped_idx]
        y_bottoms = [y_bottoms[k] for k in deduped_idx]
        n_available = len(y_tops)

        # Word-wrap and fit
        actual_size = font_size
        wrapped = wrap_hebrew_text(translated, heb_font, actual_size, block_width)
        while len(wrapped) > n_available and actual_size > MIN_FONT_SIZE:
            actual_size *= 0.92
            wrapped = wrap_hebrew_text(translated, heb_font, actual_size, block_width)
        wrapped = wrapped[:n_available]

        if n_available >= 2:
            line_spacing = (y_tops[-1] - y_tops[0]) / (n_available - 1)
        else:
            line_spacing = actual_size * 1.2

        for j, wline in enumerate(wrapped):
            wline_clean = ''.join(ch for ch in wline if ch.isprintable() or ch == ' ')
            try:
                visual = get_display(wline_clean)
            except Exception:
                visual = wline_clean
            tl = heb_font.text_length(visual, fontsize=actual_size)
            x_pos = max(0, block_x1 - tl)
            if j < len(y_tops):
                y_pos = y_bottoms[j] - (y_bottoms[j] - y_tops[j]) * 0.15
            else:
                y_pos = y_bottoms[-1] + (j - len(y_tops) + 1) * line_spacing
            tw = fitz.TextWriter(page.rect)
            try:
                tw.append(fitz.Point(x_pos, y_pos), visual,
                          font=heb_font, fontsize=actual_size)
                tw.write_text(page, color=(r_c, g_c, b_c))
            except Exception as e:
                _tprint(f"  [p{page_num} Insert error: {e}]")

    # Phase 5: Paste figure region images
    for rect, png_bytes in figure_images:
        try:
            page.draw_rect(rect, color=None, fill=(1, 1, 1))
            page.insert_image(rect, stream=png_bytes, keep_proportion=True)
        except Exception as e:
            _tprint(f"  [p{page_num} Figure paste error: {e}]")


def _translate_page_worker(src_pdf_path: str, orig_page_idx: int) -> bytes:
    """Worker function: open source PDF, extract one page, translate it, return PDF bytes.

    Each worker operates on its own fitz.Document — fully thread-safe.
    """
    page_num = orig_page_idx + 1
    doc = fitz.open(src_pdf_path)
    tmp_doc = fitz.open()
    tmp_doc.insert_pdf(doc, from_page=orig_page_idx, to_page=orig_page_idx)
    # Keep source page open for equation/image capture
    source_page = doc[orig_page_idx]

    translate_page(tmp_doc[0], page_num, source_page=source_page)
    doc.close()

    pdf_bytes = tmp_doc.tobytes(garbage=4, deflate=True)
    tmp_doc.close()
    return pdf_bytes


def parse_page_range(range_str: str, max_pages: int) -> list[int]:
    """Parse page range string like '1-5' or '3,5,7-10' into list of 0-based page indices."""
    pages = set()
    for part in range_str.split(","):
        part = part.strip()
        if "-" in part:
            start, end = part.split("-", 1)
            start = max(1, int(start.strip()))
            end = min(max_pages, int(end.strip()))
            for p in range(start, end + 1):
                pages.add(p - 1)  # convert to 0-based
        else:
            p = int(part.strip())
            if 1 <= p <= max_pages:
                pages.add(p - 1)
    return sorted(pages)


def _count_total_units(pdf_path: str, pages: list[int]) -> int:
    """Upper-bound count of paragraph translations, for the progress denominator.

    Counts text blocks that pass the same filters as translate_page's Phase 1
    (non-empty, has letters, >3 chars, not a pure-equation block) but WITHOUT the
    figure-region skip or per-page paragraph dedup. The real translated count is
    therefore always <= this number, so progress never overshoots. Reuses the same
    block helpers, so it cannot drift from the classification logic.
    """
    try:
        doc = fitz.open(pdf_path)
    except Exception:
        return 0
    total = 0
    for idx in pages:
        try:
            td = doc[idx].get_text("dict", flags=fitz.TEXT_PRESERVE_WHITESPACE)
        except Exception:
            continue
        for block in td.get("blocks", []):
            if block.get("type") != 0:
                continue
            full_text = _block_full_text(block)
            if not full_text or not any(c.isalpha() for c in full_text):
                continue
            if len(full_text.replace(" ", "")) <= 3:
                continue
            if _block_has_math_font(block) and _block_alpha_count(block) < 15:
                continue
            total += 1
    doc.close()
    return total


def _save_translated(out_doc, pdf_path: str, page_range_str: str, out_override) -> str:
    """Write out_doc and return the final path. Honors --out when provided.

    Keeps the reference's crash-safe pattern: write to a .tmp sibling, then rename
    over the target; if the target is locked, fall back to a <stem>_new.pdf name.
    """
    if out_override:
        final_target = os.path.abspath(out_override)
        tmp_path = final_target + ".tmp"
        try:
            out_doc.save(tmp_path, garbage=4, deflate=True)
        except Exception:
            tmp_path = final_target + f".{int(time.time())}.tmp"
            out_doc.save(tmp_path, garbage=4, deflate=True)
        out_doc.close()

        final_path = final_target
        try:
            if os.path.exists(final_path):
                os.remove(final_path)
            os.rename(tmp_path, final_path)
        except OSError:
            root, ext = os.path.splitext(final_target)
            final_path = root + "_new" + ext
            try:
                if os.path.exists(final_path):
                    os.remove(final_path)
            except OSError:
                pass
            try:
                os.rename(tmp_path, final_path)
            except OSError:
                final_path = tmp_path
        return final_path

    # ── Standalone CLI behavior (no --out): <stem>_hebrew_p<range>.pdf ──
    import glob
    base, _ext = os.path.splitext(os.path.basename(pdf_path))
    out_dir = os.path.dirname(pdf_path)
    range_tag = page_range_str.replace(',', '_').replace(' ', '')
    output_path = os.path.join(out_dir, f"{base}_hebrew_p{range_tag}.pdf")

    tmp_path = output_path + ".tmp"
    try:
        out_doc.save(tmp_path, garbage=4, deflate=True)
    except Exception:
        tmp_path = os.path.join(out_dir, f"{base}_hebrew_p{range_tag}_{int(time.time())}.pdf.tmp")
        out_doc.save(tmp_path, garbage=4, deflate=True)
    out_doc.close()

    for pattern in [f"{base}_hebrew_*.pdf", f"{base}_hebrew_*.png"]:
        for old_file in glob.glob(os.path.join(out_dir, pattern)):
            if old_file == tmp_path:
                continue
            try:
                os.remove(old_file)
            except OSError:
                pass

    final_path = output_path
    try:
        if os.path.exists(final_path):
            os.remove(final_path)
        os.rename(tmp_path, final_path)
    except OSError:
        final_path = output_path.replace(".pdf", "_new.pdf")
        try:
            if os.path.exists(final_path):
                os.remove(final_path)
        except OSError:
            pass
        try:
            os.rename(tmp_path, final_path)
        except OSError:
            final_path = tmp_path
    return final_path


def main():
    global SYSTEM_PROMPT, _g_total
    import argparse

    parser = argparse.ArgumentParser(
        description="Translate a PDF, preserving layout (Groq/Cerebras cloud backend).",
        usage="%(prog)s <pdf_path> [page_range] [--out PATH] [--target LANG] [--workers N]",
    )
    parser.add_argument("pdf_path", nargs="?", default=None, help="Path to source PDF")
    parser.add_argument("page_range", nargs="?", default=None,
                        help="Page range (e.g. 1-5 or 3,7,10-12); default = all pages")
    parser.add_argument("--out", default=None, help="Explicit output PDF path")
    parser.add_argument("--target", default="Hebrew", help="Target language name")
    parser.add_argument("--workers", "-w", type=int, default=1,
                        help="Number of parallel page workers (default: 1)")
    args = parser.parse_args()

    num_workers = max(1, args.workers)

    pdf_path = args.pdf_path
    if not pdf_path:
        print("שגיאה: לא צוין קובץ PDF לתרגום.", file=sys.stderr)
        sys.exit(1)

    if not os.path.exists(pdf_path):
        alt = os.path.join(os.path.dirname(os.path.abspath(__file__)), pdf_path)
        if os.path.exists(alt):
            pdf_path = alt
        else:
            print(f"שגיאה: הקובץ לא נמצא: {pdf_path}", file=sys.stderr)
            sys.exit(1)
    pdf_path = os.path.abspath(pdf_path)

    # Target language drives the system prompt. Rendering stays Hebrew/David — the
    # Rust host only routes Hebrew targets to this PDF->PDF path.
    SYSTEM_PROMPT = build_system_prompt(args.target)

    # At least one cloud API key must be set (keys arrive via env vars only).
    if not API_KEYS.get("groq") and not API_KEYS.get("cerebras"):
        print("שגיאה: לא הוגדרו מפתחות API. הוסף מפתח Groq או Cerebras בהגדרות.", file=sys.stderr)
        sys.exit(1)
    available = [p for p in ("groq", "cerebras") if API_KEYS.get(p)]
    print(f"Translation providers available: {', '.join(available)}")

    try:
        doc = fitz.open(pdf_path)
        total_pages = len(doc)
        doc.close()
    except Exception as e:
        print(f"שגיאה בפתיחת ה-PDF: {e}", file=sys.stderr)
        sys.exit(1)
    print(f"Opened '{pdf_path}' ({total_pages} pages)")

    page_range_str = args.page_range or ("1-%d" % total_pages)
    pages = parse_page_range(page_range_str, total_pages)
    if not pages:
        print("לא נמצא טקסט לתרגום ב-PDF (ייתכן שזהו PDF סרוק/תמונה).", file=sys.stderr)
        sys.exit(2)

    print(f"Will translate pages: {[p+1 for p in pages]}")

    # Establish the progress denominator before any translation starts. A zero
    # count means there is no extractable text (scanned/image-only PDF).
    _g_total = _count_total_units(pdf_path, pages)
    if _g_total == 0:
        print("לא נמצא טקסט לתרגום ב-PDF (ייתכן שזהו PDF סרוק/תמונה).", file=sys.stderr)
        sys.exit(2)
    print(f"Total translatable units (upper bound): {_g_total}")

    if num_workers > 1 and len(pages) > 1:
        # ── Parallel mode ──
        actual_workers = min(num_workers, len(pages))
        print(f"Using {actual_workers} parallel workers")

        # Each worker opens the PDF independently and translates one page
        results = {}  # page_idx -> pdf_bytes
        with ThreadPoolExecutor(max_workers=actual_workers) as executor:
            future_to_page = {
                executor.submit(_translate_page_worker, pdf_path, page_idx): page_idx
                for page_idx in pages
            }
            for future in as_completed(future_to_page):
                page_idx = future_to_page[future]
                try:
                    results[page_idx] = future.result()
                    _tprint(f"  Page {page_idx + 1} done.")
                except Exception as e:
                    _tprint(f"  ERROR on page {page_idx + 1}: {e}")

        # Merge results in page order
        out_doc = fitz.open()
        for page_idx in pages:
            if page_idx in results:
                tmp = fitz.open("pdf", results[page_idx])
                out_doc.insert_pdf(tmp)
                tmp.close()
            else:
                # Fallback: include original untranslated page
                src = fitz.open(pdf_path)
                out_doc.insert_pdf(src, from_page=page_idx, to_page=page_idx)
                src.close()
    else:
        # ── Sequential mode (original behavior) ──
        doc = fitz.open(pdf_path)
        out_doc = fitz.open()
        for orig_page in pages:
            out_doc.insert_pdf(doc, from_page=orig_page, to_page=orig_page)

        for i in range(len(out_doc)):
            orig_page_idx = pages[i]
            orig_page_num = orig_page_idx + 1
            # Pass source page for equation/image capture
            translate_page(out_doc[i], orig_page_num, source_page=doc[orig_page_idx])
        doc.close()

    final_path = _save_translated(out_doc, pdf_path, page_range_str, args.out)
    # Machine-readable line the Rust host parses to learn the real output path
    # (it may differ from --out if the target was locked and we fell back to _new).
    print(f"SAVED {final_path}", flush=True)
    print(f"\nDone! Translated PDF saved to: {final_path}")
    sys.exit(0)


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:
        print(f"שגיאה בתרגום ה-PDF: {e}", file=sys.stderr)
        sys.exit(1)
