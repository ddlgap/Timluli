//! Online speech engine implemented as a hidden Google Chrome "sidecar".
//!
//! Google only serves the free Web Speech (`webkitSpeechRecognition`) backend to
//! official Chrome — embedded WebView2 gets `network` errors. So instead of running
//! recognition inside our own WebView2 window, we run a tiny local HTTP server,
//! launch the user's installed Chrome (hidden, off-screen, isolated profile) pointed
//! at a recognizer page we serve, and relay transcripts back. The page does the exact
//! same continuous recognition the old WebView2 path did; behaviour is unchanged.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

/// Shared state between the Tauri commands (which set the desired listening
/// state) and the HTTP server thread (which the Chrome page polls).
pub struct SidecarShared {
    /// Whether Timluli currently wants the engine to be listening.
    pub listening: AtomicBool,
    /// Bumped on every fresh start request so the page knows to begin a new session.
    pub seq: AtomicU64,
    /// Silence auto-stop window, mirrored from settings.
    pub silence_ms: AtomicU32,
    /// Port the local server bound to (0 until started).
    pub port: AtomicU16,
    /// Recognition language (e.g. "he-IL"), mirrored from settings.
    pub lang: Mutex<String>,
    /// Live-streaming state: the text already injected into the target field for
    /// the **current in-progress segment**. Lets `/interim` append only the new
    /// stabilized words (append-only — never rewrites/backspaces) and `/final`
    /// flush just the held-back tail. Reset on session start/end and per segment.
    pub committed: Mutex<String>,
}

impl SidecarShared {
    pub fn new() -> Self {
        Self {
            listening: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            silence_ms: AtomicU32::new(2000),
            port: AtomicU16::new(0),
            lang: Mutex::new("he-IL".into()),
            committed: Mutex::new(String::new()),
        }
    }
}

/// Mark that the engine should start a fresh listening session.
pub fn request_start(shared: &SidecarShared, lang: String, silence_ms: u32) {
    *shared.lang.lock() = lang;
    shared.silence_ms.store(silence_ms.max(500), Ordering::SeqCst);
    shared.committed.lock().clear();
    shared.seq.fetch_add(1, Ordering::SeqCst);
    shared.listening.store(true, Ordering::SeqCst);
}

/// Mark that the engine should stop listening.
pub fn request_stop(shared: &SidecarShared) {
    shared.listening.store(false, Ordering::SeqCst);
}

// ── Local HTTP server ──────────────────────────────────────────────────────

/// Bind a localhost server on a random port and spawn its accept loop on a
/// dedicated thread. Stores the chosen port in `shared.port`.
pub fn start_server(app: AppHandle, shared: Arc<SidecarShared>) {
    let server = match tiny_http::Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            log::error!("chrome sidecar: failed to bind local server: {e}");
            return;
        }
    };
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(0);
    shared.port.store(port, Ordering::SeqCst);
    log::info!("chrome sidecar: server listening on 127.0.0.1:{port}");

    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let method = req.method().clone();
            let url = req.url().to_string();
            match (&method, url.as_str()) {
                (tiny_http::Method::Get, "/")
                | (tiny_http::Method::Get, "/index.html")
                | (tiny_http::Method::Get, "/recognizer.html") => {
                    let resp = tiny_http::Response::from_string(RECOGNIZER_HTML)
                        .with_header(header("Content-Type", "text/html; charset=utf-8"));
                    let _ = req.respond(resp);
                }
                (tiny_http::Method::Get, p) if p.starts_with("/poll") => {
                    let lang = shared.lang.lock().replace('"', "");
                    let json = format!(
                        "{{\"listening\":{},\"seq\":{},\"lang\":\"{}\",\"silenceMs\":{}}}",
                        shared.listening.load(Ordering::SeqCst),
                        shared.seq.load(Ordering::SeqCst),
                        lang,
                        shared.silence_ms.load(Ordering::SeqCst)
                    );
                    let resp = tiny_http::Response::from_string(json)
                        .with_header(header("Content-Type", "application/json"))
                        .with_header(header("Cache-Control", "no-store"));
                    let _ = req.respond(resp);
                }
                (tiny_http::Method::Post, "/interim") => {
                    let body = read_body(&mut req);
                    // Bubble shows the full live preview (instant)…
                    let _ = app.emit_to("mic", "speakly://interim", body.clone());
                    // …while the field receives only the stabilized words.
                    stream_stable(&app, &shared, &body);
                    let _ = req.respond(ok_resp());
                }
                (tiny_http::Method::Post, "/final") => {
                    let body = read_body(&mut req);
                    inject_final(&app, &shared, &body);
                    let _ = req.respond(ok_resp());
                }
                (tiny_http::Method::Post, "/ready") => {
                    // The recognizer has actually begun capturing audio
                    // (webkitSpeechRecognition.onaudiostart) — flip the mic from the
                    // "connecting" wait cue to "listening" so the user knows to speak.
                    // Guard on `listening`: the user may have cancelled (toggled off)
                    // while the Google connection was still opening, in which case the
                    // mic is already idle and we must not revive it.
                    if shared.listening.load(Ordering::SeqCst) {
                        let _ = app.emit_to("mic", "speakly://state-changed", "listening");
                        let _ = app.emit_to("panel", "speakly://state-changed", "listening");
                        crate::commands::sync_side_panel_mic(&app, "listening");
                    }
                    let _ = req.respond(ok_resp());
                }
                (tiny_http::Method::Post, "/ended") => {
                    let body = read_body(&mut req);
                    on_ended(&app, &shared, &body);
                    let _ = req.respond(ok_resp());
                }
                (tiny_http::Method::Post, "/error") => {
                    let body = read_body(&mut req);
                    on_error(&app, &shared, &body);
                    let _ = req.respond(ok_resp());
                }
                _ => {
                    let _ = req.respond(
                        tiny_http::Response::from_string("not found").with_status_code(404),
                    );
                }
            }
        }
    });
}

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("static header is valid")
}

fn ok_resp() -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string("ok")
}

fn read_body(req: &mut tiny_http::Request) -> String {
    let mut s = String::new();
    let _ = req.as_reader().read_to_string(&mut s);
    s
}

/// Punctuate a finalized segment if the engine is loaded; else return it unchanged.
/// Runs synchronously on this HTTP thread (~26 ms, infrequent — only on `/final`).
/// `newlines` = start a new line after each sentence.
#[cfg(target_os = "windows")]
fn maybe_punctuate(app: &AppHandle, text: &str, newlines: bool) -> String {
    let handle = app
        .state::<AppState>()
        .punct_engine
        .lock()
        .as_ref()
        .map(std::sync::Arc::clone);
    match handle {
        Some(h) => {
            let out = h.punctuate_blocking(text, true);
            if newlines {
                crate::commands_punct::to_line_per_sentence(&out)
            } else {
                out
            }
        }
        None => text.to_string(),
    }
}

fn inject_final(app: &AppHandle, shared: &SidecarShared, text: &str) {
    if text.trim().is_empty() {
        // Still close the segment so the next one streams fresh.
        shared.committed.lock().clear();
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let state = app.state::<AppState>();
        let hwnd = *state.target_hwnd.lock();
        if let Some(h) = hwnd {
            let mut committed = shared.committed.lock();
            let result = if committed.is_empty() {
                // Nothing streamed for this segment (short phrase, the browser
                // finalized in one shot, or auto-punctuation suppressed streaming):
                // inject the whole thing via the normal path (clipboard-capable for
                // long text). When punctuation is active this is where the full
                // sentence gets its marks before injection.
                let newlines = crate::settings::load_or_init(app)
                    .map(|s| s.punctuation_newline)
                    .unwrap_or(false);
                let punctuated = maybe_punctuate(app, text, newlines);
                crate::text_injection::inject(h, &punctuated)
            } else {
                // We already streamed stabilized words; append only what the
                // final adds beyond what's committed, word-aligned so a revised
                // word isn't re-typed mid-word.
                let cut = common_word_prefix_len(committed.as_str(), text);
                crate::text_injection::inject_append(h, &text[cut..])
            };
            committed.clear();
            if let Err(e) = result {
                log::warn!("chrome sidecar: inject failed: {e}");
                let _ = app.emit_to("settings", "speakly://error", e);
            }
        } else {
            shared.committed.lock().clear();
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, text);
        shared.committed.lock().clear();
    }
}

/// Byte length of the longest common prefix of `committed` and `text` that ends
/// on a word boundary — so a `/final` flush never re-types a partial word.
/// Returns `committed.len()` when `committed` is a full prefix of `text` (the
/// common case: the final just extends what we already streamed).
fn common_word_prefix_len(committed: &str, text: &str) -> usize {
    let cb = committed.as_bytes();
    let tb = text.as_bytes();
    let max = cb.len().min(tb.len());
    let mut i = 0;
    let mut last_boundary = 0;
    while i < max && cb[i] == tb[i] {
        i += 1;
        // ASCII space is single-byte and never a UTF-8 continuation byte, so a
        // byte-level boundary check is safe for Hebrew text.
        if cb[i - 1] == b' ' {
            last_boundary = i;
        }
    }
    if i == cb.len() {
        cb.len()
    } else {
        last_boundary
    }
}

/// Inject the newly-stabilized words from an interim result, append-only. Holds
/// back the last 2 words (the tail Google most often revises) and only ever
/// appends text that extends what's already committed — never backspaces, so it
/// can't corrupt pre-existing field content.
#[allow(unused_variables)]
fn stream_stable(app: &AppHandle, shared: &SidecarShared, interim: &str) {
    let words: Vec<&str> = interim.split_whitespace().collect();
    if words.len() < 3 {
        return; // too little to trust yet — wait for more
    }
    let mut stable = words[..words.len() - 2].join(" ");
    stable.push(' ');

    #[cfg(target_os = "windows")]
    {
        // When auto-punctuation is active, don't stream words into the field live —
        // the whole sentence is punctuated and injected at /final instead (the mic
        // bubble still shows the live interim). Leaving `committed` empty routes
        // /final through the whole-segment punctuated path in inject_final.
        if app.state::<AppState>().punct_engine.lock().is_some() {
            return;
        }
        let mut committed = shared.committed.lock();
        if !stable.starts_with(committed.as_str()) {
            // A previously-committed word got revised. Stay append-only: don't
            // rewrite it; wait for the stream to converge again.
            return;
        }
        let delta: String = stable[committed.len()..].to_string();
        if delta.is_empty() {
            return;
        }
        let state = app.state::<AppState>();
        let hwnd = *state.target_hwnd.lock();
        if let Some(h) = hwnd {
            match crate::text_injection::inject_append(h, &delta) {
                Ok(()) => *committed = stable,
                Err(e) => log::warn!("chrome sidecar: stream inject failed: {e}"),
            }
        }
    }
}

/// The page finished a recognition session (silence/user stop). If it matches the
/// current generation, drop back to idle.
fn on_ended(app: &AppHandle, shared: &SidecarShared, body: &str) {
    let finished_seq: u64 = body.trim().parse().unwrap_or(0);
    if finished_seq == shared.seq.load(Ordering::SeqCst) {
        shared.listening.store(false, Ordering::SeqCst);
        shared.committed.lock().clear();
        let state = app.state::<AppState>();
        *state.is_listening.lock() = false;
        let _ = app.emit_to("mic", "speakly://state-changed", "idle");
        let _ = app.emit_to("panel", "speakly://state-changed", "idle");
        // Side-panel mode: recording ended → hide the docked mic again.
        crate::commands::sync_side_panel_mic(app, "idle");
    }
}

fn on_error(app: &AppHandle, shared: &SidecarShared, body: &str) {
    shared.listening.store(false, Ordering::SeqCst);
    shared.committed.lock().clear();
    let state = app.state::<AppState>();
    *state.is_listening.lock() = false;
    let _ = app.emit_to("mic", "speakly://state-changed", "error");
    let _ = app.emit_to("panel", "speakly://state-changed", "error");
    // Side-panel mode: recording errored out → hide the docked mic again.
    crate::commands::sync_side_panel_mic(app, "error");
    let msg = if body.contains("network") {
        "שגיאת רשת בזיהוי הדיבור (Chrome)".to_string()
    } else if body.contains("not-allowed") || body.contains("service-not-allowed") {
        "הרשאת מיקרופון נדחתה".to_string()
    } else {
        format!("שגיאת זיהוי דיבור: {}", body.trim())
    };
    let _ = app.emit_to("settings", "speakly://error", msg);
    log::warn!("chrome sidecar: speech error: {}", body.trim());
}

// ── Chrome process management ──────────────────────────────────────────────

/// Ensure a hidden Chrome sidecar is running and pointed at our server. No-op if
/// one is already alive.
pub fn ensure_chrome(app: &AppHandle, shared: &SidecarShared) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let state = app.state::<AppState>();
        let mut guard = state.chrome_child.lock();
        if let Some(child) = guard.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return Ok(()); // still running
            }
        }

        let port = shared.port.load(Ordering::SeqCst);
        if port == 0 {
            return Err("השרת המקומי של מנוע התמלול לא הופעל".into());
        }
        let chrome = find_chrome()
            .ok_or("Google Chrome לא נמצא במחשב. התקן Chrome, או עבור למנוע המקומי בהגדרות.")?;

        let profile = std::env::temp_dir().join(format!("timluli-chrome-engine-{port}"));
        let url = format!("http://127.0.0.1:{port}/");

        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let child = std::process::Command::new(&chrome)
            .arg(format!("--app={url}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--use-fake-ui-for-media-stream")
            .arg("--autoplay-policy=no-user-gesture-required")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-features=Translate,MediaRouter")
            .arg("--disable-background-timer-throttling")
            .arg("--disable-renderer-backgrounding")
            .arg("--disable-backgrounding-occluded-windows")
            .arg("--window-position=-32000,-32000")
            .arg("--window-size=360,260")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("הפעלת Chrome נכשלה: {e}"))?;
        let pid = child.id();
        *guard = Some(child);
        drop(guard);

        // Best-effort: once the recognizer window appears, strip it from the
        // taskbar and shove it off-screen so the user never sees it. We target
        // the Chrome child by PID (timing-independent, can't match the wrong
        // window) and keep re-applying for a bit, because Chrome re-asserts its
        // window state — re-adding the taskbar button — as the page finishes
        // loading. `hide_offscreen_by_pid` only does work when a window actually
        // needs re-stripping, so steady state is just a cheap window scan.
        std::thread::spawn(move || {
            let mut found_by_pid = false;
            for _ in 0..80 {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if crate::win_util::hide_offscreen_by_pid(pid) {
                    found_by_pid = true;
                } else if !found_by_pid {
                    let _ = crate::win_util::hide_offscreen_by_title("Timluli Recognizer");
                }
            }
        });
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, shared);
        Err("מנוע ה-Chrome נתמך כרגע רק ב-Windows".into())
    }
}

/// Kill the sidecar Chrome on app exit.
pub fn shutdown(app: &AppHandle) {
    let state = app.state::<AppState>();
    let child = state.chrome_child.lock().take();
    if let Some(mut child) = child {
        let _ = child.kill();
    }
}

#[cfg(target_os = "windows")]
fn find_chrome() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let rel = r"Google\Chrome\Application\chrome.exe";
    for key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Ok(base) = std::env::var(key) {
            let p = PathBuf::from(base).join(rel);
            if p.exists() {
                return Some(p);
            }
        }
    }
    chrome_from_registry().filter(|p| p.exists())
}

#[cfg(target_os = "windows")]
fn chrome_from_registry() -> Option<std::path::PathBuf> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    for root in ["HKLM", "HKCU"] {
        let key = format!(
            r"{root}\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe"
        );
        let out = std::process::Command::new("reg")
            .args(["query", &key, "/ve"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if let Ok(out) = out {
            let s = String::from_utf8_lossy(&out.stdout);
            for line in s.lines() {
                if let Some(idx) = line.find("REG_SZ") {
                    let val = line[idx + "REG_SZ".len()..].trim();
                    if !val.is_empty() {
                        return Some(std::path::PathBuf::from(val));
                    }
                }
            }
        }
    }
    None
}

// ── Recognizer page (runs inside the hidden Chrome) ────────────────────────

const RECOGNIZER_HTML: &str = r#"<!DOCTYPE html>
<html lang="he"><head><meta charset="utf-8"><title>Timluli Recognizer</title></head>
<body>
<script>
const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
let rec = null;
let running = false;
let mySeq = -1;
let desired = false;
let lang = 'he-IL';
let silenceMs = 2000;
let userRequestedStop = false;
let runStartedAt = 0;
let silenceTimer = null, initialTimer = null, quickStopTimer = null;
const INITIAL_NO_SPEECH_MS = 10000;
// How long after speech ends we force-finalize (rec.stop()), which is what makes
// the recognized tail land in the field. Derived from silenceMs on every poll
// (~half of it) so the field catches up fast without cutting off normal pauses.
let quickStopMs = 750;

function post(path, body) { try { fetch(path, { method: 'POST', body: body == null ? '' : body }); } catch (e) {} }
function clearTimers() {
  if (silenceTimer) clearTimeout(silenceTimer);
  if (initialTimer) clearTimeout(initialTimer);
  if (quickStopTimer) clearTimeout(quickStopTimer);
  silenceTimer = initialTimer = quickStopTimer = null;
}
function resetSilence() {
  if (silenceTimer) clearTimeout(silenceTimer);
  silenceTimer = setTimeout(() => { if (rec && running) { userRequestedStop = true; try { rec.stop(); } catch (e) {} } }, silenceMs);
}
function startInitial() {
  if (initialTimer) clearTimeout(initialTimer);
  initialTimer = setTimeout(() => { if (rec && running) { userRequestedStop = true; try { rec.stop(); } catch (e) {} } }, INITIAL_NO_SPEECH_MS);
}

function startRec() {
  if (!SR) { post('/error', 'no-speechrecognition'); return; }
  userRequestedStop = false;
  running = true;
  runStartedAt = Date.now();
  // Tell the host the instant audio is actually being captured, so the mic can
  // switch from the "wait" cue to "speak now". Posted at most once per session.
  let readyPosted = false;
  const markReady = () => { if (!readyPosted) { readyPosted = true; post('/ready'); } };
  rec = new SR();
  rec.lang = lang;
  rec.continuous = true;
  rec.interimResults = true;
  rec.maxAlternatives = 3;

  rec.onstart = () => { startInitial(); };
  rec.onaudiostart = () => { markReady(); };
  rec.onspeechstart = () => {
    if (initialTimer) { clearTimeout(initialTimer); initialTimer = null; }
    if (quickStopTimer) { clearTimeout(quickStopTimer); quickStopTimer = null; }
    resetSilence();
  };
  rec.onspeechend = () => {
    if (quickStopTimer) clearTimeout(quickStopTimer);
    quickStopTimer = setTimeout(() => { if (rec && running) { userRequestedStop = true; try { rec.stop(); } catch (e) {} } }, quickStopMs);
  };
  rec.onresult = (event) => {
    if (initialTimer) { clearTimeout(initialTimer); initialTimer = null; }
    resetSilence();
    let interim = '';
    for (let i = event.resultIndex; i < event.results.length; i++) {
      const result = event.results[i];
      if (result.isFinal) {
        let best = result[0];
        for (let j = 1; j < result.length; j++) {
          if ((result[j].confidence || 0) > (best.confidence || 0)) best = result[j];
        }
        const text = best.transcript;
        if (text && text.trim()) post('/final', text.trim() + ' ');
      } else {
        interim += result[0].transcript;
      }
    }
    if (interim) post('/interim', interim);
  };
  rec.onerror = (event) => { readyPosted = true; clearTimers(); running = false; post('/error', String(event.error || 'unknown')); };
  rec.onend = () => {
    const ranFor = Date.now() - runStartedAt;
    clearTimers();
    if (!userRequestedStop && running && ranFor > 1500 && desired) {
      running = false;
      setTimeout(startRec, 50);
      return;
    }
    running = false;
    post('/ended', String(mySeq));
  };

  try { rec.start(); } catch (err) { running = false; post('/error', String(err)); }
  // Watchdog: if neither onaudiostart nor onerror fires within 4 s, unblock the UI
  // anyway so the mic never sticks on "wait" (a real failure already posts /error).
  setTimeout(() => { if (running && !readyPosted) markReady(); }, 4000);
}

async function poll() {
  try {
    const r = await fetch('/poll', { cache: 'no-store' });
    const s = await r.json();
    desired = !!s.listening;
    if (s.lang) lang = s.lang;
    if (s.silenceMs) silenceMs = s.silenceMs;
    quickStopMs = Math.max(400, Math.round(silenceMs * 0.5));
    if (s.listening) {
      if (s.seq !== mySeq && !running) { mySeq = s.seq; startRec(); }
    } else {
      if (running && !userRequestedStop) { userRequestedStop = true; try { rec && rec.stop(); } catch (e) {} }
    }
  } catch (e) {}
  setTimeout(poll, 120);
}
poll();
</script>
</body></html>
"#;
