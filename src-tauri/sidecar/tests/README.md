# PDF translation pipeline — tests

Two layers, matching the two kinds of failure the pipeline can have.

## 1. `test_helpers.py` — fast, no-API, CI-friendly

Pure-function regression tests for the deterministic layer: token protection (§5),
response sanitation / `<think>` + foreign-script rejection (§11), the telecom glossary
and gated banned-term correction (§9), numbered-list detection/splitting (§6), and the
QA gate (§13, using synthetic in-memory PDFs). No network, runs in <1s.

```powershell
& .build-venv\Scripts\python.exe tests\test_helpers.py      # or: pytest tests\test_helpers.py
```

These guard against regressions like the `_fix_primes` sentinel-mangling bug
(`Q0Q9Z` → `Q0Q'Z`) and the David-font נ-drop heuristics.

## 2. Fixture translation — needs API keys, dev-only

The end-to-end render/QA checks translate real PDFs and therefore need a `GROQ_API_KEY`
(and optionally `CEREBRAS_API_KEY`) plus the fixture PDFs, which live outside the repo
(see the `reference_translation_testing` project memory). The harness lives in
`.claude/temp/pdf-qa/` (`run_batch1.py`, `run_tests.py`): it translates each fixture in
`same-box` and `mirror-columns`, writes `<out>.qa.json` (the QA-gate report), renders
every page to PNG, and checks English-layer leakage + telecom terminology.

```powershell
$env:GROQ_API_KEY = "gsk_..."
$env:PYTHONIOENCODING = "utf-8"
& .build-venv\Scripts\python.exe ..\..\.claude\temp\pdf-qa\run_batch1.py
```

The QA gate also runs automatically inside the sidecar on every translation and writes
`<output>.qa.json`; a mirror render that fails (English leakage, clipping, missing נ,
leaked sentinel) auto-falls back to same-box.
