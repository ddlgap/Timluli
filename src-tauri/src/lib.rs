use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;

mod commands;
mod commands_local;
mod models;
mod settings;
mod shortcut;
mod tray;
mod whisper_local;

#[cfg(target_os = "windows")]
mod text_injection;
#[cfg(target_os = "windows")]
mod win_util;

pub struct AppState {
    pub target_hwnd: Mutex<Option<isize>>,
    pub is_listening: Mutex<bool>,
    pub muted: Mutex<bool>,
    /// Loaded local whisper engine (None until user loads a model).
    /// Arc allows cloning the handle out of the Mutex before awaiting.
    pub local_engine: Mutex<Option<Arc<whisper_local::LocalEngineHandle>>>,
    /// Active download cancellation tokens, keyed by model id.
    pub active_downloads: Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            target_hwnd: Mutex::new(None),
            is_listening: Mutex::new(false),
            muted: Mutex::new(false),
            local_engine: Mutex::new(None),
            active_downloads: Mutex::new(HashMap::new()),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(shortcut::build_plugin())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            // ── existing 20 commands (order preserved) ──
            commands::capture_target_window,
            commands::start_listening,
            commands::stop_listening,
            commands::toggle_listening,
            commands::inject_text,
            commands::inject_partial,
            commands::report_interim,
            commands::report_state,
            commands::report_error,
            commands::get_settings,
            commands::save_settings,
            commands::update_shortcut,
            commands::pause_global_shortcut,
            commands::resume_global_shortcut,
            commands::set_autostart_enabled,
            commands::open_settings,
            commands::toggle_mute,
            commands::set_mic_visible,
            commands::quit_app,
            commands::store_mic_position,
            // ── new local-engine commands (appended) ──
            commands_local::list_engines,
            commands_local::set_active_engine,
            commands_local::list_models,
            commands_local::download_model,
            commands_local::cancel_download,
            commands_local::delete_model,
            commands_local::verify_model,
            commands_local::import_model_manual,
            commands_local::load_local_model,
            commands_local::unload_local_model,
            commands_local::transcribe_local,
        ])
        .setup(|app| {
            let stg = settings::load_or_init(app.handle())
                .unwrap_or_else(|_| settings::Settings::default());

            // Auto-load the whisper model that was active in the previous session.
            if stg.engine_id == "whisper-local" {
                if let Some(model_id) = stg.local_model_id.clone() {
                    let handle = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        commands_local::autoload_model(&handle, model_id).await;
                    });
                }
            }

            #[cfg(target_os = "windows")]
            {
                if let Some(mic) = app.get_webview_window("mic") {
                    win_util::make_topmost_noactivate(&mic);
                }
            }

            if let Some(mic) = app.get_webview_window("mic") {
                if let Some(pos) = stg.mic_position.as_ref() {
                    let _ = mic.set_position(tauri::Position::Physical(
                        tauri::PhysicalPosition::new(pos.x, pos.y),
                    ));
                }
                if !stg.show_mic_on_startup {
                    let _ = mic.hide();
                }
            }

            // Show onboarding on first run
            if !stg.onboarding_done {
                if let Some(onboarding) = app.get_webview_window("onboarding") {
                    let _ = onboarding.show();
                    let _ = onboarding.set_focus();
                }
            }

            tray::create(app, &stg)?;
            shortcut::register_initial(app.handle(), &stg.shortcut)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Don't actually quit when individual windows close; hide them.
                let label = window.label();
                if label == "settings" || label == "mic" || label == "onboarding" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Timluli");
}
