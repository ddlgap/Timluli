"""Pure-geometry unit tests for the RTL layout layer of timluli_pdf.py.

These exercise the layout math (frame estimation, mirroring, centering, collision
veto, and the same-box regression guarantee) WITHOUT rendering a PDF or hitting the
network. Run from the build venv, where PyMuPDF (fitz) is installed:

    .build-venv/Scripts/python.exe src-tauri/sidecar/test_layout.py

Exit 0 = all assertions pass; any failure raises AssertionError (exit 1).
"""

import fitz

import timluli_pdf as T


# A4-ish page used throughout (PyMuPDF default points).
PAGE = fitz.Rect(0, 0, 595, 842)


def _unit(x0, x1, y0, y1, alpha_count=40, is_mathy=False):
    """Minimal unit dict carrying exactly the keys the geometry layer reads."""
    return {
        "block_x0": x0,
        "block_x1": x1,
        "block_y0": y0,
        "block_y1": y1,
        "alpha_count": alpha_count,
        "is_mathy": is_mathy,
    }


def test_mirror_rect_in_frame():
    """A left-hugging rect reflects to hug the right edge; width and y preserved."""
    frame_x0, frame_x1 = 50.0, 550.0
    rect = fitz.Rect(50, 100, 150, 130)  # left edge, width 100
    m = T.mirror_rect_in_frame(rect, frame_x0, frame_x1)
    # new_x0 = 50+550-150 = 450 ; new_x1 = 50+550-50 = 550
    assert abs(m.x0 - 450.0) < 1e-6, m.x0
    assert abs(m.x1 - 550.0) < 1e-6, m.x1
    assert abs(m.width - rect.width) < 1e-6, m.width
    assert m.y0 == rect.y0 and m.y1 == rect.y1, (m.y0, m.y1)
    # Reflecting a centered rect is a no-op.
    centered = fitz.Rect(250, 0, 350, 10)  # centered in [50,550]
    mc = T.mirror_rect_in_frame(centered, frame_x0, frame_x1)
    assert abs(mc.x0 - centered.x0) < 1e-6 and abs(mc.x1 - centered.x1) < 1e-6


def test_centered_title_not_mirrored():
    """A centered heading is detected as centered and left in place by mirror-text."""
    page_w = PAGE.width
    # Symmetric margins around page center (595/2 = 297.5).
    title = fitz.Rect(200, 40, 395, 70)
    assert T.is_centered_rect(title, page_w) is True
    u = _unit(200, 395, 40, 70, alpha_count=20)
    assert T.should_mirror_unit(u) is True  # text-wise safe...
    T.compute_target_rects([u], PAGE, [], "mirror-text")
    # ...but centered, so it must keep its source box.
    assert u["target_x0"] == 200 and u["target_x1"] == 395


def test_formula_not_mirrored():
    """A math-ish, off-center block is never mirrored."""
    u = _unit(60, 200, 300, 320, alpha_count=5, is_mathy=True)
    assert T.should_mirror_unit(u) is False
    T.compute_target_rects([u], PAGE, [], "mirror-text")
    assert u["target_x0"] == 60 and u["target_x1"] == 200


def test_short_label_not_mirrored():
    """A tiny label (few alpha chars) is excluded from mirroring."""
    u = _unit(60, 90, 400, 412, alpha_count=2)
    assert T.should_mirror_unit(u) is False
    T.compute_target_rects([u], PAGE, [], "mirror-text")
    assert u["target_x0"] == 60 and u["target_x1"] == 90


def test_body_paragraph_mirrored():
    """An off-center body paragraph gets a target rect different from its source."""
    # Two units establish a content frame; the narrow left-column one should move.
    wide = _unit(60, 535, 100, 200, alpha_count=400)   # full-width body
    left = _unit(60, 250, 220, 260, alpha_count=60)    # left-aligned narrow block
    T.compute_target_rects([wide, left], PAGE, [], "mirror-text")
    assert left["target_x0"] != 60 or left["target_x1"] != 250, left
    # It should have moved rightward (new x0 greater than original).
    assert left["target_x0"] > 60, left["target_x0"]
    # Width preserved.
    assert abs((left["target_x1"] - left["target_x0"]) - (250 - 60)) < 1e-6


def test_collision_falls_back_to_same_box():
    """When a mirrored target overlaps a figure too much, the unit stays same-box."""
    # Narrow left block that, when mirrored, lands on top of a right-side figure.
    block = _unit(60, 200, 300, 360, alpha_count=60)
    other = _unit(60, 535, 100, 260, alpha_count=400)  # frame anchor (wide body)
    # Figure occupying the right half where the mirror would land.
    figure = fitz.Rect(380, 280, 540, 380)
    T.compute_target_rects([block, other], PAGE, [figure], "mirror-text")
    # block should have fallen back to its source box.
    assert block["target_x0"] == 60 and block["target_x1"] == 200, block


def test_same_box_regression():
    """same-box mode yields target == source for every unit (zero behavior change)."""
    units = [
        _unit(60, 200, 100, 130, alpha_count=60),
        _unit(300, 500, 100, 130, alpha_count=60, is_mathy=True),
        _unit(60, 90, 400, 412, alpha_count=2),
    ]
    T.compute_target_rects(units, PAGE, [], "same-box")
    for u in units:
        assert u["target_x0"] == u["block_x0"], u
        assert u["target_x1"] == u["block_x1"], u


def test_get_content_frame_ignores_tiny_units():
    """Page numbers / tiny labels must not stretch the content frame."""
    body = _unit(70, 520, 100, 700, alpha_count=500)
    page_no = _unit(290, 305, 800, 812, alpha_count=2)  # tiny, centered footer
    fx0, fx1 = T.get_content_frame([body, page_no], PAGE)
    # Frame should track the body block, not the tiny footer.
    assert abs(fx0 - 70) < 1e-6, fx0
    assert abs(fx1 - 520) < 1e-6, fx1


def test_side_by_side_detection():
    """Two cells sharing a row are side-by-side; stacked blocks are not."""
    left = _unit(54, 293, 524, 545, alpha_count=63)   # question cell
    right = _unit(311, 386, 524, 545, alpha_count=15)  # label cell, same row
    assert T._units_side_by_side(left, right) is True
    # Vertically stacked single-column blocks must NOT count as side-by-side.
    top = _unit(70, 520, 100, 150, alpha_count=300)
    bottom = _unit(70, 520, 160, 210, alpha_count=300)
    assert T._units_side_by_side(top, bottom) is False


def test_multicolumn_page_keeps_same_box():
    """A table page (side-by-side cells) stays same-box even in mirror-text mode."""
    units = [
        _unit(54, 293, 524, 545, alpha_count=63),   # question (left column)
        _unit(311, 386, 524, 545, alpha_count=15),  # label (right column)
        _unit(54, 313, 322, 343, alpha_count=40),   # another left-column row
    ]
    assert T.page_is_multicolumn(units) is True
    T.compute_target_rects(units, PAGE, [], "mirror-text")
    for u in units:
        assert u["target_x0"] == u["block_x0"], u
        assert u["target_x1"] == u["block_x1"], u


def test_single_column_page_not_multicolumn():
    """Stacked single-column blocks are not flagged, so mirroring still applies."""
    wide = _unit(60, 535, 100, 200, alpha_count=400)
    left = _unit(60, 250, 220, 260, alpha_count=60)
    assert T.page_is_multicolumn([wide, left]) is False


# A 2-column table spanning the page, divider at x=306 (the layout we debugged).
TABLE = [{"x0": 48.0, "y0": 250.0, "x1": 564.0, "y1": 620.0,
          "edges": [48.0, 306.0, 564.0]}]


def test_mirror_column_band():
    """Reflecting a column band about the table center swaps left<->right."""
    assert T.mirror_column_band(48, 306, 48, 564) == (306, 564)
    assert T.mirror_column_band(306, 564, 48, 564) == (48, 306)


def test_find_unit_column():
    """Units are assigned to their column; full-width units return None."""
    left = _unit(54, 293, 524, 545, alpha_count=63)
    right = _unit(311, 386, 524, 545, alpha_count=15)
    full = _unit(50, 558, 300, 320, alpha_count=400)   # spans both columns
    outside = _unit(54, 293, 100, 130, alpha_count=40)  # above the table (y<250)

    lc = T.find_unit_column(left, TABLE)
    rc = T.find_unit_column(right, TABLE)
    assert lc is not None and lc[1] == (48.0, 306.0), lc
    assert rc is not None and rc[1] == (306.0, 564.0), rc
    assert T.find_unit_column(full, TABLE) is None
    assert T.find_unit_column(outside, TABLE) is None


def test_table_columns_swapped_in_mirror_text():
    """A table's columns swap: left-column cells move right, right-column cells left."""
    q = _unit(54, 293, 524, 545, alpha_count=63)    # left column (question)
    lbl = _unit(311, 386, 524, 545, alpha_count=15)  # right column (label)
    T.compute_target_rects([q, lbl], PAGE, [], "mirror-text", TABLE)
    # Question moved into the RIGHT column (its target now sits right of center).
    assert q["target_x0"] > 306, q
    assert abs(q["target_x0"] - 312) < 1.0 and abs(q["target_x1"] - 558) < 1.0, q
    # Label moved into the LEFT column (target now left of center).
    assert lbl["target_x1"] < 306, lbl
    assert abs(lbl["target_x0"] - 53) < 1.0 and abs(lbl["target_x1"] - 301) < 1.0, lbl


def test_table_cell_wrap_width_is_column_not_textbox():
    """A wide left cell that overflowed before now wraps to the mirrored column width."""
    # Source text bbox (54..344) is wider than its column and used to overflow.
    wide_q = _unit(54, 344, 594, 612, alpha_count=42)
    T.compute_target_rects([wide_q], PAGE, [], "mirror-text", TABLE)
    width = wide_q["target_x1"] - wide_q["target_x0"]
    # Target width is bounded by the column width (~258), not the 290-wide source box.
    assert width <= 258, width
    assert wide_q["target_x1"] <= 564, wide_q


def _run():
    tests = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    for t in tests:
        t()
        print(f"  PASS {t.__name__}")
    print(f"\nAll {len(tests)} layout tests passed.")


if __name__ == "__main__":
    _run()
