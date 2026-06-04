# Bundled Hebrew font (optional, recommended for redistribution)

The PDF renderer (`timluli_pdf.py`, `_resolve_hebrew_font`) prefers a bundled font here
over the system font, so the app never depends on an unsafe system font and stays
machine-independent.

Drop these two files in this directory and rebuild the sidecar:

- `NotoSansHebrew-Regular.ttf`
- `NotoSansHebrew-Bold.ttf`

Noto Sans Hebrew is licensed under the SIL Open Font License (OFL) and is freely
redistributable. Get the static TTFs from:
<https://fonts.google.com/noto/specimen/Noto+Sans+Hebrew> (Download family) or the
`notofonts/hebrew` GitHub repo.

If these files are absent, the renderer falls back to **Arial** (always present on
Windows and verified to round-trip all 27 Hebrew letters including נ — unlike David,
which silently drops נ and must never be used). `build_sidecar.ps1` bundles this
directory into the exe automatically when a `.ttf` is present.

After adding the font, verify with the round-trip test:

```powershell
& ..\.build-venv\Scripts\python.exe ..\tests\test_helpers.py
```
