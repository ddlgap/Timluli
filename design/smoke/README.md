# Settings-window design migration — smoke harness

A safety net that lets us migrate the settings UI to the new design **step by step**,
proving at each step that the code behaves **identically** to the current working app —
without a Rust backend and without breaking the JS↔Rust pipeline.

## What it does

Runs the **real** `src/settings.html` + `src/scripts/settings.js` in a plain browser
with a mocked Tauri layer ([tauri-mock.js](tauri-mock.js)). It:

1. **Renders the full visual state** (real markup + real CSS) — screenshot-verifiable.
2. **Records every `invoke`** the frontend fires (the backend contract).
3. **Detects JS errors** (`window.__SMOKE__.errors`).

The captured baseline is [golden-contract.json](golden-contract.json). It already
surfaced one command (`get_ffmpeg_status`) that a code read missed — exactly the kind
of silent drop this harness exists to catch.

## How to run (chrome-devtools MCP)

Vite serves the real files (`npm run dev` → http://127.0.0.1:1420). Then:

1. `navigate_page({ type:'url', url:'http://127.0.0.1:1420/settings.html', initScript: <contents of tauri-mock.js> })`
   — `initScript` runs before the page scripts, so the mock is ready when `@tauri-apps/api` loads.
2. Verify boot: `evaluate_script` → `window.__SMOKE__.commands()` and `window.__SMOKE__.errors`.
3. Drive each control and capture its contract:
   ```js
   const S = window.__SMOKE__, wait = ms => new Promise(r=>setTimeout(r,ms));
   S.reset();
   document.querySelector('input[name="engine_id"][value="whisper-local"]').dispatchEvent(new Event('change',{bubbles:true}));
   await wait(140);
   S.calls.map(c => c.cmd);   // → must equal golden-contract.json["engine -> whisper-local"]
   ```
4. Screenshot to confirm the visual state.

## The migration loop (per step — zero-break protocol)

Work on a branch. One small change per step (e.g. one tab restructured).

1. Apply the step (frontend only — never touch Rust / `settings.rs` / commands).
2. Re-run the harness against the changed code.
3. **Assert, against `golden-contract.json`:**
   - boot command set unchanged (no command dropped),
   - every interaction fires the same command set (same names, same order),
   - `errors === []`,
   - the new UI renders (screenshot).
4. Green → commit the step. Red → fix before proceeding.
5. Repeat for the next tab.

## Boundary (honest)

The harness mocks the JS↔Rust boundary: it proves the frontend **calls** the right
commands with the right shape and renders. It does **not** execute Rust or real
WebView2 focus/HWND behavior. So the final gate is a single `npm run tauri:dev`
manual pass over every control in the real app. The harness removes ~95% of the risk
(dropped commands, wrong args, broken wiring, render breaks) automatically; the manual
pass covers the rest.
