use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{
    Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
};

use crate::AppState;

pub fn build_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = handle_pressed(app).await {
                    log::warn!("shortcut handling failed: {err}");
                }
            });
        })
        .build()
}

pub(crate) async fn handle_pressed(app: AppHandle) -> Result<(), String> {
    // Hold a separate handle so `state` can keep its borrow alive
    // while `app` is moved into the inner command call.
    let borrow_owner = app.clone();
    let state = borrow_owner.state::<AppState>();
    if *state.muted.lock() {
        return Ok(());
    }
    let listening = *state.is_listening.lock();
    if listening {
        crate::commands::stop_listening(app, state).await
    } else {
        crate::commands::start_listening(app, state).await
    }
}

/// Make `combo` the active trigger. A double-tap gesture (`"Ctrl+Ctrl"`,
/// `"Alt+Alt"`, …) is driven by the low-level keyboard detector, since the
/// global-shortcut API can only bind modifier+key chords. Anything else is a
/// regular `RegisterHotKey` chord.
fn activate(app: &AppHandle, combo: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if let Some(code) = crate::double_tap::parse_target(combo) {
            crate::double_tap::set_target(code);
            return Ok(());
        }
        // Not a double-tap gesture → make sure the detector is idle.
        crate::double_tap::disable();
    }
    let shortcut = parse_shortcut(combo)?;
    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| e.to_string())
}

/// Undo whatever `combo` was driving: turn the detector off for a double-tap
/// gesture, or unregister the chord otherwise.
fn deactivate(app: &AppHandle, combo: &str) {
    #[cfg(target_os = "windows")]
    {
        if crate::double_tap::is_double_tap(combo) {
            crate::double_tap::disable();
            return;
        }
    }
    if let Ok(s) = parse_shortcut(combo) {
        let _ = app.global_shortcut().unregister(s);
    }
}

pub fn register_initial(app: &AppHandle, combo: &str) -> Result<(), String> {
    if let Err(e) = activate(app, combo) {
        log::warn!("failed to register shortcut '{combo}': {e}");
        let _ = app.emit_to("settings", "speakly://shortcut-conflict", combo);
    }
    Ok(())
}

pub fn reregister(app: &AppHandle, old_combo: &str, new_combo: &str) -> Result<(), String> {
    deactivate(app, old_combo);
    activate(app, new_combo)
}

#[allow(dead_code)]
pub fn unregister(app: &AppHandle, combo: &str) -> Result<(), String> {
    deactivate(app, combo);
    Ok(())
}

pub fn unregister_all(app: &AppHandle) {
    let _ = app.global_shortcut().unregister_all();
    #[cfg(target_os = "windows")]
    crate::double_tap::disable();
}

pub fn register_combo(app: &AppHandle, combo: &str) -> Result<(), String> {
    activate(app, combo)
}

fn parse_shortcut(combo: &str) -> Result<Shortcut, String> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for raw in combo.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "win" | "super" | "meta" | "cmd" | "command" => mods |= Modifiers::META,
            other => {
                code = Some(map_code(other)?);
            }
        }
    }
    let code = code.ok_or_else(|| format!("Missing key in shortcut: {combo}"))?;
    Ok(Shortcut::new(Some(mods), code))
}

fn map_code(raw: &str) -> Result<Code, String> {
    use Code::*;
    let key = match raw {
        "space" => Space,
        "enter" | "return" => Enter,
        "tab" => Tab,
        "backspace" => Backspace,
        "escape" | "esc" => Escape,
        "f1" => F1, "f2" => F2, "f3" => F3, "f4" => F4, "f5" => F5, "f6" => F6,
        "f7" => F7, "f8" => F8, "f9" => F9, "f10" => F10, "f11" => F11, "f12" => F12,
        "a" => KeyA, "b" => KeyB, "c" => KeyC, "d" => KeyD, "e" => KeyE,
        "f" => KeyF, "g" => KeyG, "h" => KeyH, "i" => KeyI, "j" => KeyJ,
        "k" => KeyK, "l" => KeyL, "m" => KeyM, "n" => KeyN, "o" => KeyO,
        "p" => KeyP, "q" => KeyQ, "r" => KeyR, "s" => KeyS, "t" => KeyT,
        "u" => KeyU, "v" => KeyV, "w" => KeyW, "x" => KeyX, "y" => KeyY, "z" => KeyZ,
        "0" => Digit0, "1" => Digit1, "2" => Digit2, "3" => Digit3, "4" => Digit4,
        "5" => Digit5, "6" => Digit6, "7" => Digit7, "8" => Digit8, "9" => Digit9,
        "," => Comma,
        "." => Period,
        ";" => Semicolon,
        "/" => Slash,
        other => return Err(format!("Unknown key: {other}")),
    };
    Ok(key)
}
