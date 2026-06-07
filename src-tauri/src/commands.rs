use crate::settings::{self, MicPosition, Settings};
use crate::shortcut;
use crate::AppState;
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(target_os = "windows")]
use crate::{text_injection, win_util};

/// Emits a UI event to both the floating mic and the side panel. Only one of the
/// two is ever visible (per `display_mode`), so the hidden one simply ignores it.
/// Used for events both display modes consume: state changes, interim text, and
/// translate/transcribe done/error. `serde::Serialize + Clone` mirrors `emit_to`.
fn emit_ui<S: serde::Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to("mic", event, payload.clone());
    let _ = app.emit_to("panel", event, payload);
}

/// Broadcasts a `speakly://state-changed` like `emit_ui`, plus side-panel mic
/// management: in side-panel mode the floating mic is hidden at rest and only
/// shown — docked to the active dictation field — while recording (`listening`/
/// `processing`), then hidden again on `idle`/`error`. In floating-mic mode this
/// is just `emit_ui` (the mic is always visible). Use this for every state change.
fn emit_state(app: &AppHandle, state: &str) {
    emit_ui(app, "speakly://state-changed", state);
    sync_side_panel_mic(app, state);
}

/// Shows/hides the floating mic for the "transient" display modes — `side-panel`
/// and `hidden-mic` — based on the dictation state. In both, the mic is hidden at
/// rest and revealed only while recording (`listening`/`processing`), then hidden
/// on `idle`/`error`. `side-panel` docks it to the active dictation field;
/// `hidden-mic` shows it at the user's saved position. No-op in `floating-mic`
/// (the mic is always visible there). Callable from any thread (also invoked by
/// the Chrome sidecar's HTTP handler on `idle`/`error`).
pub(crate) fn sync_side_panel_mic(app: &AppHandle, state: &str) {
    #[cfg(target_os = "windows")]
    {
        let Ok(stg) = settings::load_or_init(app) else {
            return;
        };
        let dock_to_field = stg.display_mode == "side-panel";
        let transient = dock_to_field || stg.display_mode == "hidden-mic";
        if !transient {
            return;
        }
        let Some(mic) = app.get_webview_window("mic") else {
            return;
        };
        match state {
            // Recording just started: position the mic, then reveal it.
            "listening" => {
                // The positioning happens once, on the transition into recording.
                // Both the command and (for the local engine) the renderer report
                // "listening"; if the mic is already shown, don't re-position it
                // (the position is frozen for the duration of a recording).
                if mic.is_visible().unwrap_or(false) {
                    return;
                }
                let pos = if dock_to_field {
                    // Side-panel: dock to the field being dictated into. The mic
                    // is centered in its (now 240px) window, so shift the top-left
                    // up-left by half the size growth to keep the visible disc snug
                    // to the field — same docked spot as the old 160px window.
                    const PAD: i32 = 8;
                    let d = (40.0 * mic.scale_factor().unwrap_or(1.0)).round() as i32;
                    crate::field_tracker::focused_field_rect(app)
                        .map(|r| (r.right + PAD - d, r.top - d))
                        .or_else(|| crate::panel::default_mic_pos(app))
                } else if stg.field_docking_enabled {
                    // Hidden mode + field-docking on: dock centered above the focused
                    // field (same placement as the floating-mic tracker), so it honors
                    // "attach the mic to the active field" like the other modes. Fall
                    // back to the saved floating position when no editable field is found.
                    crate::field_tracker::focused_field_rect(app)
                        .map(|r| crate::field_tracker::dock_position(&mic, r))
                        .or_else(|| stg.mic_position.map(|p| (p.x, p.y)))
                        .or_else(|| crate::panel::default_mic_pos(app))
                } else {
                    // Hidden mode, docking off: appear at the user's saved floating position.
                    stg.mic_position
                        .map(|p| (p.x, p.y))
                        .or_else(|| crate::panel::default_mic_pos(app))
                };
                if let Some((x, y)) = pos {
                    let _ = mic.set_position(tauri::Position::Physical(
                        tauri::PhysicalPosition::new(x, y),
                    ));
                }
                let _ = mic.show();
                win_util::make_topmost_noactivate(&mic);
            }
            // Transcribing/injecting the final result: keep it visible (don't move).
            "processing" => {
                let _ = mic.show();
                win_util::make_topmost_noactivate(&mic);
            }
            // idle / error / anything else: back to hidden.
            _ => {
                let _ = mic.hide();
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, state);
    }
}

#[tauri::command]
pub fn capture_target_window(state: State<AppState>) -> Result<isize, String> {
    #[cfg(target_os = "windows")]
    {
        let hwnd = win_util::get_foreground_hwnd();
        *state.target_hwnd.lock() = Some(hwnd);
        Ok(hwnd)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = state;
        Err("Windows-only".into())
    }
}

#[tauri::command]
pub async fn start_listening(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if *state.muted.lock() {
        return Err("Timluli is muted".into());
    }
    if *state.is_listening.lock() {
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let hwnd = win_util::get_foreground_hwnd();
        *state.target_hwnd.lock() = Some(hwnd);
    }

    *state.is_listening.lock() = true;

    let stg = settings::load_or_init(&app).unwrap_or_else(|_| Settings::default());

    if stg.engine_id == "web-speech" {
        // Online engine: drive the hidden Chrome sidecar instead of the WebView2
        // speech window (Google blocks Web Speech for embedded WebView2).
        crate::chrome_sidecar::request_start(
            &state.sidecar,
            stg.language.clone(),
            stg.silence_timeout_ms,
        );
        if let Err(e) = crate::chrome_sidecar::ensure_chrome(&app, &state.sidecar) {
            *state.is_listening.lock() = false;
            crate::chrome_sidecar::request_stop(&state.sidecar);
            emit_state(&app, "error");
            let _ = app.emit_to("settings", "speakly://error", &e);
            return Err(e);
        }
        emit_state(&app, "listening");
    } else {
        app.emit_to("speech", "speakly://start-listening", ())
            .map_err(|e| e.to_string())?;
        emit_state(&app, "listening");
    }
    Ok(())
}

#[tauri::command]
pub async fn stop_listening(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    *state.is_listening.lock() = false;
    let stg = settings::load_or_init(&app).unwrap_or_else(|_| Settings::default());
    if stg.engine_id == "web-speech" {
        crate::chrome_sidecar::request_stop(&state.sidecar);
    } else {
        let _ = app.emit_to("speech", "speakly://stop-listening", ());
    }
    emit_state(&app, "idle");
    Ok(())
}

#[tauri::command]
pub async fn toggle_listening(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let listening = *state.is_listening.lock();
    if listening {
        stop_listening(app, state).await
    } else {
        start_listening(app, state).await
    }
}

#[tauri::command]
pub async fn inject_text(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
) -> Result<(), String> {
    *state.is_listening.lock() = false;
    let hwnd_opt = *state.target_hwnd.lock();

    emit_state(&app, "processing");

    #[cfg(target_os = "windows")]
    {
        if let Some(hwnd) = hwnd_opt {
            text_injection::inject(hwnd, &text)?;
        } else {
            return Err("No target window captured".into());
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (hwnd_opt, text);
    }

    emit_state(&app, "idle");
    Ok(())
}

#[tauri::command]
pub async fn inject_partial(state: State<'_, AppState>, text: String) -> Result<(), String> {
    let hwnd_opt = *state.target_hwnd.lock();
    #[cfg(target_os = "windows")]
    {
        if let Some(hwnd) = hwnd_opt {
            text_injection::inject(hwnd, &text)?;
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (hwnd_opt, text);
    }
    Ok(())
}

#[tauri::command]
pub fn report_interim(app: AppHandle, text: String) -> Result<(), String> {
    emit_ui(&app, "speakly://interim", text);
    Ok(())
}

#[tauri::command]
pub fn report_state(
    app: AppHandle,
    app_state: State<'_, AppState>,
    state: String,
) -> Result<(), String> {
    // Keep is_listening in sync when the renderer reports idle/error so the next
    // shortcut press starts a fresh recognition instead of toggling off a stale flag.
    if state == "idle" || state == "error" {
        *app_state.is_listening.lock() = false;
    }
    emit_state(&app, &state);
    Ok(())
}

#[tauri::command]
pub fn report_error(app: AppHandle, message: String) -> Result<(), String> {
    emit_state(&app, "error");
    let _ = app.emit_to("settings", "speakly://error", &message);
    log::error!("speech error: {message}");
    Ok(())
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<Settings, String> {
    settings::load_or_init(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(app: AppHandle, new_settings: Settings) -> Result<(), String> {
    settings::save(&app, &new_settings).map_err(|e| e.to_string())?;
    let _ = app.emit_to("mic", "speakly://settings-changed", &new_settings);
    let _ = app.emit_to("speech", "speakly://settings-changed", &new_settings);
    Ok(())
}

#[tauri::command]
pub fn update_shortcut(app: AppHandle, combo: String) -> Result<(), String> {
    let mut stg = settings::load_or_init(&app).map_err(|e| e.to_string())?;
    shortcut::reregister(&app, &stg.shortcut, &combo).map_err(|e| e.to_string())?;
    stg.shortcut = combo;
    settings::save(&app, &stg).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn pause_global_shortcut(app: AppHandle) -> Result<(), String> {
    shortcut::unregister_all(&app);
    Ok(())
}

#[tauri::command]
pub fn resume_global_shortcut(app: AppHandle) -> Result<(), String> {
    let stg = settings::load_or_init(&app).map_err(|e| e.to_string())?;
    shortcut::register_combo(&app, &stg.shortcut)
}

#[tauri::command]
pub fn set_autostart_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| e.to_string())?;
    } else {
        mgr.disable().map_err(|e| e.to_string())?;
    }
    let mut stg = settings::load_or_init(&app).map_err(|e| e.to_string())?;
    stg.start_with_windows = enabled;
    settings::save(&app, &stg).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.unminimize();
    }
    Ok(())
}

#[tauri::command]
pub fn toggle_mute(app: AppHandle, state: State<'_, AppState>) -> Result<bool, String> {
    let mut muted = state.muted.lock();
    *muted = !*muted;
    let value = *muted;
    let _ = app.emit_to(
        "mic",
        "speakly://state-changed",
        if value { "muted" } else { "idle" },
    );
    Ok(value)
}

#[tauri::command]
pub fn set_mic_visible(app: AppHandle, visible: bool) -> Result<(), String> {
    if let Some(mic) = app.get_webview_window("mic") {
        if visible {
            let _ = mic.show();
        } else {
            let _ = mic.hide();
        }
    }
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub fn store_mic_position(
    app: AppHandle,
    state: State<AppState>,
    x: i32,
    y: i32,
) -> Result<(), String> {
    // While field-docking is active, the mic's position is computed from the
    // focused field. Don't overwrite the user's manual fallback position with
    // auto-docked coordinates. Likewise in side-panel mode the mic is auto-docked
    // to the dictation field while recording, so a stray drag must not persist.
    #[cfg(target_os = "windows")]
    {
        if state.field_tracker.lock().is_some() {
            return Ok(());
        }
        if settings::load_or_init(&app)
            .map(|s| s.display_mode == "side-panel")
            .unwrap_or(false)
        {
            return Ok(());
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = state;
    }
    let mut stg = settings::load_or_init(&app).map_err(|e| e.to_string())?;
    stg.mic_position = Some(MicPosition { x, y });
    settings::save(&app, &stg).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn set_field_docking(
    app: AppHandle,
    state: State<AppState>,
    enabled: bool,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // In side-panel and hidden modes there is no follow tracker (the mic only
        // appears while recording, via sync_side_panel_mic), so this toggle does
        // nothing now. The preference is still persisted (for when the user returns
        // to floating-mic) by the regular settings save.
        if settings::load_or_init(&app)
            .map(|s| s.display_mode == "side-panel" || s.display_mode == "hidden-mic")
            .unwrap_or(false)
        {
            return Ok(());
        }
        let mut slot = state.field_tracker.lock();
        if enabled && slot.is_none() {
            *slot = Some(crate::field_tracker::FieldTrackerHandle::start(app.clone(), false));
        } else if !enabled && slot.is_some() {
            *slot = None; // Drop signals shutdown
            // Restore the user's manual fallback position so the mic doesn't
            // freeze in the last docked spot.
            drop(slot);
            if let Ok(stg) = settings::load_or_init(&app) {
                if let Some(pos) = stg.mic_position {
                    if let Some(mic) = app.get_webview_window("mic") {
                        let _ = mic.set_position(tauri::Position::Physical(
                            tauri::PhysicalPosition::new(pos.x, pos.y),
                        ));
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, state, enabled);
    }
    Ok(())
}

// ─── Document translation ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn translate_file(app: AppHandle, path: String) -> Result<String, String> {
    let result = crate::translation::translate_file(&app, &path).await;
    match &result {
        Ok(out) => {
            emit_ui(&app, "speakly://translate-done", out.clone());
        }
        Err(e) => {
            emit_ui(&app, "speakly://translate-error", e.clone());
        }
    }
    result
}

// ─── Audio-file transcription ───────────────────────────────────────────────────

/// Transcribes an audio file dragged onto the mic, saving the transcript as
/// `<stem>.txt` next to the source. Backend (`groq` / `whisper-local`) is chosen
/// by `settings.audio_file_engine`.
#[tauri::command]
pub async fn transcribe_audio_file(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    let result = crate::transcription::transcribe_audio_file(&app, state, &path).await;
    match &result {
        Ok(out) => {
            emit_ui(&app, "speakly://transcribe-done", out.clone());
        }
        Err(e) => {
            emit_ui(&app, "speakly://transcribe-error", e.clone());
        }
    }
    result
}

/// Persists the audio-file transcription backend (`groq` | `whisper-local`).
#[tauri::command]
pub fn set_audio_file_engine(app: AppHandle, engine_id: String) -> Result<(), String> {
    let mut stg = settings::load_or_init(&app)?;
    stg.audio_file_engine = engine_id;
    settings::save(&app, &stg)
}

#[tauri::command]
pub fn save_translation_keys(
    app: AppHandle,
    groq: Option<String>,
    cerebras: Option<String>,
) -> Result<(), String> {
    crate::secrets::save_keys(&app, groq, cerebras)
}

#[tauri::command]
pub fn get_translation_keys_status(app: AppHandle) -> Result<crate::secrets::KeyStatus, String> {
    Ok(crate::secrets::status(&app))
}

/// Lists a provider's available chat/text models (live from its `/models`
/// endpoint) for the translation settings model picker. Requires a saved key.
#[tauri::command]
pub async fn list_provider_models(
    app: AppHandle,
    provider: String,
    key: Option<String>,
) -> Result<Vec<crate::translation::ModelInfo>, String> {
    crate::translation::list_models(&app, &provider, key).await
}

// ─── Display mode (floating mic ⇄ side panel) ───────────────────────────────────

/// Switches the UI display mode and swaps the visible window accordingly. Persists
/// the choice and broadcasts `speakly://display-mode-changed` so other windows
/// (e.g. settings) can sync. `mode` is `"side-panel"`, `"floating-mic"`, or
/// `"hidden-mic"` (mic hidden at rest, shown only while recording).
#[tauri::command]
pub fn set_display_mode(app: AppHandle, state: State<AppState>, mode: String) -> Result<(), String> {
    let mut stg = settings::load_or_init(&app)?;
    stg.display_mode = mode.clone();
    settings::save(&app, &stg)?;

    if mode == "side-panel" {
        if let Some(mic) = app.get_webview_window("mic") {
            let _ = mic.hide();
        }
        crate::panel::show_panel(&app);
        // No follow tracker in side-panel mode: the mic is hidden at rest and only
        // appears (docked to the active field) while recording — see
        // sync_side_panel_mic. Drop any floating-mic field-docking tracker and the
        // topmost keeper (the mic is hidden at rest; nothing to keep on top).
        #[cfg(target_os = "windows")]
        {
            *state.field_tracker.lock() = None;
            *state.topmost_keeper.lock() = None;
        }
    } else if mode == "hidden-mic" {
        // Hidden mode: no panel, mic hidden at rest. It appears only while recording
        // (sync_side_panel_mic on the listening transition) and hides on idle.
        crate::panel::hide_panel(&app);
        if let Some(mic) = app.get_webview_window("mic") {
            let _ = mic.hide();
        }
        #[cfg(target_os = "windows")]
        {
            *state.field_tracker.lock() = None;
            *state.topmost_keeper.lock() = None;
        }
    } else {
        crate::panel::hide_panel(&app);
        if let Some(mic) = app.get_webview_window("mic") {
            let _ = mic.show();
            #[cfg(target_os = "windows")]
            win_util::make_topmost_noactivate(&mic);
        }
        // Back to floating-mic: drop the auto-hide tracker, then re-enable classic
        // field-docking only if the user had it on.
        #[cfg(target_os = "windows")]
        {
            *state.field_tracker.lock() = None;
            if stg.field_docking_enabled {
                *state.field_tracker.lock() =
                    Some(crate::field_tracker::FieldTrackerHandle::start(app.clone(), false));
            }
            // Floating-mic is the only always-visible mode: keep it reliably on top
            // by re-asserting topmost periodically. Overwriting drops any previous
            // keeper (clean shutdown via Drop).
            *state.topmost_keeper.lock() =
                Some(crate::topmost_keeper::TopmostKeeperHandle::start(app.clone()));
        }
    }

    emit_ui(&app, "speakly://display-mode-changed", mode.clone());
    let _ = app.emit_to("settings", "speakly://display-mode-changed", mode);
    Ok(())
}

/// Expands or collapses the side panel (resize + reposition). No-op visually when
/// the panel window isn't the active display mode.
#[tauri::command]
pub fn set_panel_expanded(app: AppHandle, expanded: bool) -> Result<(), String> {
    crate::panel::position_panel(&app, expanded);
    Ok(())
}

/// Clips a window (`mic` or `panel`) to a circle so clicks outside it pass through
/// to the desktop — removing the transparent dead-zone around the visible widget.
/// `cx`/`cy`/`r` are physical pixels relative to the window's top-left (the JS
/// caller multiplies CSS coords by `devicePixelRatio`). A circle centered on the
/// window's right edge yields the side-panel's left-facing semicircle.
#[tauri::command]
pub fn set_circle_region(app: AppHandle, label: String, cx: f64, cy: f64, r: f64) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if let Some(win) = app.get_webview_window(&label) {
            let left = (cx - r).round() as i32;
            let top = (cy - r).round() as i32;
            let right = (cx + r).round() as i32;
            let bottom = (cy + r).round() as i32;
            win_util::set_ellipse_region(&win, left, top, right, bottom);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, label, cx, cy, r);
    }
    Ok(())
}

/// Restores a window's full rectangular hit area (used while the mic's bubble or
/// context menu needs the whole window).
#[tauri::command]
pub fn clear_hit_region(app: AppHandle, label: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if let Some(win) = app.get_webview_window(&label) {
            win_util::clear_region(&win);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, label);
    }
    Ok(())
}

/// Persists the panel's vertical position (analogous to `store_mic_position`, but
/// only Y — X is always anchored to the right work-area edge).
#[tauri::command]
pub fn store_panel_offset_y(app: AppHandle, y: i32) -> Result<(), String> {
    let mut stg = settings::load_or_init(&app).map_err(|e| e.to_string())?;
    stg.panel_offset_y = Some(y);
    settings::save(&app, &stg).map_err(|e| e.to_string())?;
    Ok(())
}

/// Opens an http(s) URL in the user's default browser. The webview intercepts
/// `target="_blank"` navigation, so external links must round-trip through here.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("כתובת לא נתמכת".into());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", &url])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = url;
    }
    Ok(())
}
