use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, State,
};

use crate::settings::Settings;
use crate::AppState;

pub fn create(app: &mut App, _settings: &Settings) -> tauri::Result<()> {
    let handle = app.handle();

    let settings_item =
        MenuItem::with_id(handle, "settings", "הגדרות", true, None::<&str>)?;
    let mute_item = MenuItem::with_id(handle, "toggle_mute", "השתק / הפעל", true, None::<&str>)?;
    let toggle_mic_item =
        MenuItem::with_id(handle, "toggle_mic", "הצג / הסתר מיקרופון", true, None::<&str>)?;
    let about_item = MenuItem::with_id(handle, "about", "אודות Timluli", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(handle)?;
    let quit_item = MenuItem::with_id(handle, "quit", "יציאה", true, None::<&str>)?;

    let menu = Menu::with_items(
        handle,
        &[
            &settings_item,
            &mute_item,
            &toggle_mic_item,
            &separator,
            &about_item,
            &quit_item,
        ],
    )?;

    let icon = handle
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("tray icon".into()))?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(false)
        .tooltip("Timluli — תמלול בעברית")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                toggle_mic_visibility(app);
            }
        })
        .build(handle)?;

    Ok(())
}

fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "settings" => {
            if let Some(w) = app.get_webview_window("settings") {
                let _ = w.show();
                let _ = w.set_focus();
                let _ = w.unminimize();
            }
        }
        "toggle_mute" => {
            let state: State<AppState> = app.state::<AppState>();
            let _ = crate::commands::toggle_mute(app.clone(), state);
        }
        "toggle_mic" => toggle_mic_visibility(app),
        "about" => {
            if let Some(w) = app.get_webview_window("settings") {
                let _ = w.show();
                let _ = w.set_focus();
                let _ = w.eval("window.location.hash = '#about';");
            }
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

fn toggle_mic_visibility(app: &AppHandle) {
    // In side-panel mode the mic is hidden and the panel is the active window;
    // toggle whichever one the current display mode uses so left-click and
    // "show/hide" stay meaningful in both modes.
    let side_panel = crate::settings::load_or_init(app)
        .map(|s| s.display_mode == "side-panel")
        .unwrap_or(false);

    let label = if side_panel { "panel" } else { "mic" };
    if let Some(win) = app.get_webview_window(label) {
        let visible = win.is_visible().unwrap_or(false);
        if visible {
            let _ = win.hide();
        } else if side_panel {
            // Re-show docked to the right edge with the correct geometry.
            crate::panel::show_panel(app);
        } else {
            let _ = win.show();
            #[cfg(target_os = "windows")]
            crate::win_util::make_topmost_noactivate(&win);
        }
    }
}
