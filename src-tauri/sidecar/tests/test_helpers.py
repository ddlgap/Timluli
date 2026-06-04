"""Fast, no-API regression tests for the deterministic layers of the PDF translation
pipeline (timluli_pdf.py): token protection, response sanitation, glossary, numbered-
list detection, and the QA gate. These never hit the network, so they run in CI in
well under a second.

Run directly:        python tests/test_helpers.py
Or with pytest:      pytest tests/test_helpers.py
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))  # import timluli_pdf from the parent dir
import timluli_pdf as T  # noqa: E402

try:
    import fitz  # noqa: E402
except Exception:
    fitz = None


# ── §5 token protection ──────────────────────────────────────────────────────
def test_protect_masks_all_token_kinds():
    text = ("Press Redial key on Yealink T57W, dial *802, call 1-800-555-1234, "
            "visit www.GoMomentum.com/support, email a@b.com, firmware v1.2.3, model AX83H")
    masked, mp = T._protect_tokens(text)
    vals = set(mp.values())
    for tok in ["Yealink", "T57W", "*802", "1-800-555-1234",
                "www.GoMomentum.com/support", "a@b.com", "v1.2.3", "AX83H"]:
        assert tok in vals, f"{tok} not protected; map={mp}"
    # no sentinel was re-masked (single-pass invariant)
    import re
    assert not [v for v in vals if re.fullmatch(r"Q\d+Q9Z", v)]


def test_protect_does_not_mask_plain_words_or_refs():
    masked, mp = T._protect_tokens("See Figure 1-4 and Equation 1-3, the QUICK START GUIDE, USB DND PIN")
    assert mp == {}, mp


def test_sentinel_survives_output_prime_cleanup():
    # The regression: _fix_primes (input rule) would turn Q0Q9Z -> Q0Q'Z and break restore.
    masked, mp = T._protect_tokens("Yealink T57W")
    model_out = " ".join(mp.keys())               # model echoes the sentinels
    cleaned = T._fix_primes_output(model_out)      # output-side cleanup must NOT mangle
    restored = T._restore_tokens(cleaned, mp)
    assert restored.strip() == "Yealink T57W", restored
    assert "Q9Z" not in restored and "Q'" not in restored


def test_defragment_urls():
    assert T._defragment_urls("www. GoM omentum.c .com/support").replace(" ", "") \
        .startswith("www.GoMomentum")


# ── §11 response validation ──────────────────────────────────────────────────
def test_strip_reasoning():
    assert T._strip_reasoning("<think>plan</think>שלום") == "שלום"
    assert T._strip_reasoning("שלום<think>noise") == "שלום"
    assert T._strip_reasoning("<think>truncated שלום") == ""
    assert T._strip_reasoning("no tags שלום") == "no tags שלום"


def test_foreign_script_and_validity():
    assert T._has_foreign_script("你好") and T._has_foreign_script("สวัสดี")
    assert not T._has_foreign_script("שלום world T57W")
    assert T._is_valid_hebrew_output("שלום עולם")
    assert not T._is_valid_hebrew_output("hello only")
    assert not T._is_valid_hebrew_output("שלום 你好")
    assert not T._is_valid_hebrew_output("<think>x</think>שלום")


# ── §9 glossary ──────────────────────────────────────────────────────────────
def test_static_glossary_loads():
    g = T._load_static_glossary()
    assert g.get("extension") == "שלוחה"
    assert g.get("paging") == "כריזה"
    assert g.get("speakerphone") == "דיבורית"
    assert len(g) > 30


def test_glossary_banned_term_correction_is_gated():
    heb, fixes = T._enforce_glossary("Dial the extension number", "חייג את מספר התוסף")
    assert heb == "חייג את מספר השלוחה" and fixes == ["תוסף→שלוחה"]
    # not triggered without the English trigger in the same unit
    heb2, fixes2 = T._enforce_glossary("Install a software plugin", "התקן תוסף תוכנה")
    assert heb2 == "התקן תוסף תוכנה" and fixes2 == []


# ── §6 numbered lists ────────────────────────────────────────────────────────
def test_numbered_list_detection():
    assert T._is_numbered_list("1.Transfer key—x. 2.Hold key—y. 3.Voicemail—z.")
    assert T._is_numbered_list("1. a 2. b 3. c 4. d")
    assert not T._is_numbered_list("resolution 3.5 x 2.0 inches")
    assert not T._is_numbered_list("see step 1. and 5.")  # not consecutive


def test_numbered_list_split():
    lead, items = T._split_numbered_items("Options: 1.First 2.Second 3.Third")
    assert lead == "Options:" and [n for n, _ in items] == ["1", "2", "3"]
    assert items[0][1] == "First"


# ── §13 QA gate (synthetic PDFs, no API) ─────────────────────────────────────
def _make_pdf(path, text, size=(612, 792)):
    doc = fitz.open()
    page = doc.new_page(width=size[0], height=size[1])
    page.insert_text((72, 144), text, fontsize=12)
    doc.save(path)
    doc.close()


def test_qa_gate_flags_untranslated_and_sentinels(tmpdir=None):
    if fitz is None:
        print("  (skipping QA-gate test: fitz unavailable)")
        return
    import tempfile
    d = tempfile.mkdtemp()
    src = os.path.join(d, "src.pdf")
    # source: a paragraph of English
    _make_pdf(src, "This is a long English source paragraph about telephones and calls. " * 4)

    # good Hebrew output
    good = os.path.join(d, "good.pdf")
    _make_pdf(good, "זוהי פסקה ארוכה בעברית על טלפונים ושיחות נכנסות ויוצאות במשרד. " * 4)
    rep = T.run_qa_gate(src, good, "same-box")
    assert rep["status"] in ("passed", "warning"), rep
    assert rep["nun_total"] >= 0

    # leaked sentinel must hard-fail
    bad = os.path.join(d, "bad.pdf")
    _make_pdf(bad, "טקסט עברי עם דליפת אסימון Q0Q9Z שנשאר בפנים והרבה מילים נוספות כאן. " * 4)
    rep2 = T.run_qa_gate(src, bad, "same-box")
    assert rep2["sentinel_leaks"], rep2
    assert rep2["status"] == "failed", rep2


# ── §4 font round-trip ───────────────────────────────────────────────────────
def test_font_roundtrip_all_hebrew_letters():
    """The active Hebrew font (bundled Noto if present, else Arial) must render EVERY
    Hebrew letter so that it survives render→extract — especially נ (U+05E0), which the
    banned David font silently drops. This validates whatever font is actually active."""
    if fitz is None:
        print("  (skipping font round-trip: fitz unavailable)")
        return
    import tempfile
    alphabet = "אבגדהוזחטיכךלמםנןסעפףצץקרשת"  # 22 letters + 5 final forms
    doc = fitz.open()
    page = doc.new_page(width=320, height=220)
    arch = fitz.Archive(T.HEB_FONT_DIR)
    html = f'<div style="direction:rtl;font-size:20pt">{alphabet}</div>'
    page.insert_htmlbox(fitz.Rect(10, 10, 310, 210), html, css=T.HEB_CSS, archive=arch)
    out = os.path.join(tempfile.mkdtemp(), "font.pdf")
    doc.save(out)
    doc.close()
    extracted = fitz.open(out)[0].get_text()
    assert "נ" in extracted, "נ (U+05E0) missing after render→extract — unsafe font!"
    missing = [c for c in alphabet if c not in extracted]
    assert not missing, f"letters lost after render→extract: {missing} (font={T.HEB_FONT_LABEL})"


def _run_all():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    passed = 0
    for fn in fns:
        try:
            fn()
            print(f"  PASS {fn.__name__}")
            passed += 1
        except AssertionError as e:
            print(f"  FAIL {fn.__name__}: {e}")
        except Exception as e:
            print(f"  ERROR {fn.__name__}: {e!r}")
    print(f"\n{passed}/{len(fns)} passed")
    return passed == len(fns)


if __name__ == "__main__":
    sys.exit(0 if _run_all() else 1)
