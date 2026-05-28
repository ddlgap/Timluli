use crate::settings::{self, MicPosition, Settings};
use crate::shortcut;
use crate::AppState;
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(target_os = "windows")]
use crate::{text_injection, win_util};

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
            let _ = app.emit_to("mic", "speakly://state-changed", "error");
            let _ = app.emit_to("settings", "speakly://error", &e);
            return Err(e);
        }
        let _ = app.emit_to("mic", "speakly://state-changed", "listening");
    } else {
        app.emit_to("speech", "speakly://start-listening", ())
            .map_err(|e| e.to_string())?;
        app.emit_to("mic", "speakly://state-changed", "listening")
            .map_err(|e| e.to_string())?;
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
    let _ = app.emit_to("mic", "speakly://state-changed", "idle");
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

    let _ = app.emit_to("mic", "speakly://state-changed", "processing");

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

    let _ = app.emit_to("mic", "speakly://state-changed", "idle");
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
    app.emit_to("mic", "speakly://interim", text)
        .map_err(|e| e.to_string())
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
    app.emit_to("mic", "speakly://state-changed", state)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn report_error(app: AppHandle, message: String) -> Result<(), String> {
    let _ = app.emit_to("mic", "speakly://state-changed", "error");
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
pub fn store_mic_position(app: AppHandle, x: i32, y: i32) -> Result<(), String> {
    let mut stg = settings::load_or_init(&app).map_err(|e| e.to_string())?;
    stg.mic_position = Some(MicPosition { x, y });
    settings::save(&app, &stg).map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Document translation ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn translate_file(app: AppHandle, path: String) -> Result<String, String> {
    let result = crate::translation::translate_file(&app, &path).await;
    match &result {
        Ok(out) => {
            let _ = app.emit_to("mic", "speakly://translate-done", out.clone());
        }
        Err(e) => {
            let _ = app.emit_to("mic", "speakly://translate-error", e.clone());
        }
    }
    result
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
