use crate::models::types::{DownloadProgress, EngineInfo, ModelView};
use crate::models::{manager, storage};
use crate::AppState;
use base64::Engine as _;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

/// Called from lib.rs setup to auto-load the previously active model on startup.
pub async fn autoload_model(app: &tauri::AppHandle, id: String) {
    let model_dir = storage::model_dir(app, &id);
    let meta_path = model_dir.join("meta.json");
    let Ok(meta_str) = std::fs::read_to_string(&meta_path) else { return };
    let Ok(meta) = serde_json::from_str::<crate::models::types::InstalledModel>(&meta_str) else { return };
    let model_path = std::path::PathBuf::from(&meta.file_path);
    let model_id = id.clone();
    let engine = match tokio::task::spawn_blocking(move || {
        crate::whisper_local::inference::WhisperEngine::load(&model_path, model_id)
    })
    .await
    {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => { log::error!("auto-load model {id}: {e}"); return; }
        Err(e) => { log::error!("auto-load spawn error: {e}"); return; }
    };
    let state: tauri::State<crate::AppState> = app.state();
    *state.local_engine.lock() = Some(std::sync::Arc::new(
        crate::whisper_local::LocalEngineHandle::new(engine),
    ));
    log::info!("auto-loaded local model: {id}");
}

// ─── Engine selection ────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_engines(state: State<'_, AppState>) -> Vec<EngineInfo> {
    let engine_loaded = state.local_engine.lock().is_some();
    vec![
        EngineInfo {
            id: "web-speech".into(),
            display_name: "Web Speech (מקוון)".into(),
            ready: true,
        },
        EngineInfo {
            id: "whisper-local".into(),
            display_name: "מקומי (אופליין)".into(),
            ready: engine_loaded,
        },
    ]
}

#[tauri::command]
pub fn set_active_engine(
    app: AppHandle,
    _state: State<'_, AppState>,
    engine_id: String,
) -> Result<(), String> {
    let mut stg = crate::settings::load_or_init(&app).map_err(|e| e.to_string())?;
    stg.engine_id = engine_id.clone();
    crate::settings::save(&app, &stg).map_err(|e| e.to_string())?;
    let payload = serde_json::json!({ "engineId": engine_id });
    let _ = app.emit_to("mic", "speakly://engine-changed", &payload);
    let _ = app.emit_to("speech", "speakly://engine-changed", &payload);
    Ok(())
}

// ─── Model listing / verification / deletion ─────────────────────────────────

#[tauri::command]
pub fn list_models(app: AppHandle) -> Result<Vec<ModelView>, String> {
    let stg = crate::settings::load_or_init(&app).map_err(|e| e.to_string())?;
    Ok(manager::merge_view(&app, stg.local_model_id.as_deref()))
}

#[tauri::command]
pub async fn verify_model(app: AppHandle, id: String) -> Result<bool, String> {
    Ok(manager::verify(&app, &id).await)
}

#[tauri::command]
pub fn delete_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // Cannot delete the currently-loaded model without unloading first.
    let active_id = state
        .local_engine
        .lock()
        .as_ref()
        .map(|e| e.model_id.clone());
    if active_id.as_deref() == Some(&id) {
        return Err("לא ניתן למחוק מודל פעיל. בטל את הפעלתו תחילה.".into());
    }
    manager::delete(&app, &id).map_err(|e| e.to_string())
}

// ─── Download with progress ───────────────────────────────────────────────────

#[tauri::command]
pub async fn download_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    on_progress: tauri::ipc::Channel<DownloadProgress>,
) -> Result<(), String> {
    let token = CancellationToken::new();
    state
        .active_downloads
        .lock()
        .insert(id.clone(), token.clone());

    let app_clone = app.clone();
    let id_clone = id.clone();

    tokio::spawn(async move {
        let result =
            manager::download(app_clone.clone(), id_clone.clone(), on_progress, token).await;
        // Clean up the token entry regardless of outcome.
        {
            let state = app_clone.state::<AppState>();
            state.active_downloads.lock().remove(&id_clone);
        }
        if let Err(e) = result {
            log::error!("download error for {id_clone}: {e}");
            let _ = app_clone.emit_to("settings", "speakly://error", e.to_string());
        }
    });

    Ok(())
}

#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>, id: String) -> Result<(), String> {
    if let Some(token) = state.active_downloads.lock().remove(&id) {
        token.cancel();
    }
    Ok(())
}

// ─── Model loading / unloading ────────────────────────────────────────────────

#[tauri::command]
pub async fn load_local_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let model_dir = storage::model_dir(&app, &id);
    let meta_path = model_dir.join("meta.json");
    let meta_str = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("שגיאה בקריאת מטא-נתוני מודל: {e}"))?;
    let meta: crate::models::types::InstalledModel = serde_json::from_str(&meta_str)
        .map_err(|e| format!("שגיאה בפענוח מטא-נתוני מודל: {e}"))?;

    let model_path = std::path::PathBuf::from(&meta.file_path);
    let model_id = id.clone();

    // Model loading is blocking (memory-maps ~700MB–1.5GB) — use spawn_blocking.
    let engine =
        tokio::task::spawn_blocking(move || {
            crate::whisper_local::inference::WhisperEngine::load(&model_path, model_id)
        })
        .await
        .map_err(|e| format!("שגיאת thread בטעינת מנוע: {e}"))?
        .map_err(|e| e.to_string())?;

    let handle = std::sync::Arc::new(crate::whisper_local::LocalEngineHandle::new(engine));
    *state.local_engine.lock() = Some(handle);

    // Persist the active model id in settings.
    let mut stg = crate::settings::load_or_init(&app).map_err(|e| e.to_string())?;
    stg.local_model_id = Some(id);
    crate::settings::save(&app, &stg).map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub fn unload_local_model(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    *state.local_engine.lock() = None;
    let mut stg = crate::settings::load_or_init(&app).map_err(|e| e.to_string())?;
    stg.local_model_id = None;
    crate::settings::save(&app, &stg).map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Manual import ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn import_model_manual(
    app: AppHandle,
    file_path: String,
    display_name: String,
) -> Result<ModelView, String> {
    manager::import_manual(app, file_path, display_name)
        .await
        .map_err(|e| e.to_string())
}

// ─── Transcription ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn transcribe_local(
    app: AppHandle,
    state: State<'_, AppState>,
    samples_b64: String,
) -> Result<(), String> {
    // Decode base64 → raw bytes → f32 LE samples.
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&samples_b64)
        .map_err(|e| format!("שגיאה בפענוח base64: {e}"))?;

    if bytes.len() % 4 != 0 {
        return Err("אורך בלתי תקין של נתוני שמע (לא מכפלה של 4)".into());
    }
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    crate::whisper_local::audio::validate(&samples)?;

    // Pad with 0.5 s of silence so Whisper generates paired segment timestamps
    // and does not discard the last segment ("single timestamp ending - skip").
    let mut samples = samples;
    samples.extend(std::iter::repeat(0.0_f32).take(8_000));

    // Clone the Arc so we can drop the parking_lot lock before awaiting.
    let engine_arc = {
        let guard = state.local_engine.lock();
        match guard.as_ref() {
            Some(e) => std::sync::Arc::clone(e),
            None => {
                return Err(
                    "לא נטען מנוע תמלול מקומי. אנא טען מודל בהגדרות → מנוע תמלול.".into(),
                )
            }
        }
    };

    let hwnd_opt = *state.target_hwnd.lock();

    // Bug #3: re-validate HWND after the (potentially long) inference window.
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd_opt {
        if !crate::win_util::is_window(hwnd) {
            return Err("חלון היעד נסגר במהלך התמלול".into());
        }
    }

    // Language MUST be "he" (ISO-639-1), not "he-IL" or "hebrew".
    let text = engine_arc
        .transcribe(samples, "he")
        .await
        .map_err(|e| e.to_string())?;

    if text.is_empty() {
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(hwnd) = hwnd_opt {
            crate::text_injection::inject(hwnd, &text)?;
        } else {
            return Err("לא נלכד חלון יעד".into());
        }
    }

    let _ = app; // used implicitly for error routing via report_error in speech.js
    Ok(())
}
