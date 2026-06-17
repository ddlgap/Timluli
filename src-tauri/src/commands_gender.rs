//! Acoustic gender-classifier commands: status, on-demand model download (reuses the
//! `DownloadProgress` channel shape from the whisper model manager), enable/disable
//! with lazy load/unload, and startup autoload. The model runs in-process (see
//! `src/gender_onnx.rs`) and, when loaded, augments the always-on F0 path during the
//! video → SRT gender pass. Mirrors `commands_punct.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::gender_onnx::GenderEngineHandle;
use crate::models::types::DownloadProgress;
use crate::{settings, AppState};

// Hosted as a GitHub release asset on ddlgap/Timluli (matches the punctuation/ffmpeg
// artifact pattern). SHA-256 + size are from the validated quantized Wav2Vec2 model
// (Common-Voice gender, Apache-2.0). NOTE: upload `model_quantized.onnx` to the
// `gender-model-v1` release as `gender-model.onnx` for this URL to resolve.
const MODEL_URL: &str =
    "https://github.com/ddlgap/Timluli/releases/download/gender-model-v1/gender-model.onnx";
const MODEL_SHA256: &str = "a0934c2f8934878f264dd5072d07b1bbd176f051d285b2a7910dfaa173747695";
const MODEL_SIZE: u64 = 95_387_498;

const DOWNLOAD_KEY: &str = "gender";

fn gender_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("gender")
}
fn model_path(app: &AppHandle) -> PathBuf {
    gender_dir(app).join("model.onnx")
}
fn is_installed(app: &AppHandle) -> bool {
    model_path(app).exists()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenderStatus {
    pub installed: bool,
    pub loaded: bool,
    pub enabled: bool,
    pub downloading: bool,
}

#[tauri::command]
pub fn get_gender_status(app: AppHandle, state: State<'_, AppState>) -> Result<GenderStatus, String> {
    let enabled = settings::load_or_init(&app)
        .map(|s| s.gender_classifier_enabled)
        .unwrap_or(false);
    Ok(GenderStatus {
        installed: is_installed(&app),
        loaded: state.gender_engine.lock().is_some(),
        enabled,
        downloading: state.active_downloads.lock().contains_key(DOWNLOAD_KEY),
    })
}

/// Load the model into `AppState` (blocking load on a `spawn_blocking` thread).
async fn load_engine(app: &AppHandle) -> Result<(), String> {
    let mp = model_path(app);
    if !mp.exists() {
        return Err("מודל המגדר לא מותקן. הורד אותו תחילה בהגדרות.".into());
    }
    let handle = tokio::task::spawn_blocking(move || GenderEngineHandle::load(&mp))
        .await
        .map_err(|e| format!("שגיאת thread בטעינת מנוע המגדר: {e}"))??;
    *app.state::<AppState>().gender_engine.lock() = Some(Arc::new(handle));
    log::info!("gender classifier loaded");
    Ok(())
}

#[tauri::command]
pub async fn set_gender_classifier_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut stg = settings::load_or_init(&app)?;
    stg.gender_classifier_enabled = enabled;
    settings::save(&app, &stg)?;

    if enabled {
        let loaded = app.state::<AppState>().gender_engine.lock().is_some();
        if !loaded {
            load_engine(&app).await?;
        }
    } else {
        // Free the model RAM when disabled.
        *app.state::<AppState>().gender_engine.lock() = None;
    }
    let _ = app.emit_to("settings", "speakly://gender-status-changed", enabled);
    Ok(())
}

/// Called from lib.rs setup: if the classifier is enabled and installed, load it in
/// the background so the first video has no warm-up.
pub async fn autoload_gender(app: &AppHandle) {
    if app.state::<AppState>().gender_engine.lock().is_some() {
        return;
    }
    let mp = model_path(app);
    if !mp.exists() {
        return;
    }
    match tokio::task::spawn_blocking(move || GenderEngineHandle::load(&mp)).await {
        Ok(Ok(h)) => {
            *app.state::<AppState>().gender_engine.lock() = Some(Arc::new(h));
            log::info!("auto-loaded gender classifier");
        }
        Ok(Err(e)) => log::error!("gender auto-load: {e}"),
        Err(e) => log::error!("gender auto-load thread: {e}"),
    }
}

// ─── Download ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn download_gender_model(
    app: AppHandle,
    state: State<'_, AppState>,
    on_progress: Channel<DownloadProgress>,
    // When true, turn the feature on (persist + load the engine) once the model is
    // verified and installed. Defaults to false (the settings toggle drives enabling).
    enable_on_finish: Option<bool>,
) -> Result<(), String> {
    let enable_on_finish = enable_on_finish.unwrap_or(false);
    let token = CancellationToken::new();
    state
        .active_downloads
        .lock()
        .insert(DOWNLOAD_KEY.to_string(), token.clone());

    let app_clone = app.clone();
    tokio::spawn(async move {
        let result = do_download(&app_clone, on_progress, token).await;
        app_clone
            .state::<AppState>()
            .active_downloads
            .lock()
            .remove(DOWNLOAD_KEY);
        match result {
            Ok(()) => {
                if enable_on_finish {
                    if let Ok(mut stg) = settings::load_or_init(&app_clone) {
                        if !stg.gender_classifier_enabled {
                            stg.gender_classifier_enabled = true;
                            let _ = settings::save(&app_clone, &stg);
                        }
                    }
                    autoload_gender(&app_clone).await;
                }
                let _ = app_clone.emit_to("settings", "speakly://gender-model-installed", ());
                let _ = app_clone.emit_to("onboarding", "speakly://gender-model-installed", ());
            }
            Err(e) => {
                log::error!("gender model download: {e}");
                let _ = app_clone.emit_to("settings", "speakly://error", e);
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_gender_download(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(token) = state.active_downloads.lock().remove(DOWNLOAD_KEY) {
        token.cancel();
    }
    Ok(())
}

async fn do_download(
    app: &AppHandle,
    on_progress: Channel<DownloadProgress>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let dir = gender_dir(app);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;

    let total = MODEL_SIZE;
    let mut done: u64 = 0;
    let model_tmp = dir.join("model.onnx.part");

    download_one(MODEL_URL, &model_tmp, MODEL_SHA256, &on_progress, &cancel, &mut done, total).await?;

    let _ = on_progress.send(DownloadProgress {
        id: DOWNLOAD_KEY.into(),
        downloaded_bytes: total,
        total_bytes: total,
        speed_bps: 0,
    });
    tokio::fs::rename(&model_tmp, model_path(app))
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_one(
    url: &str,
    dest_tmp: &PathBuf,
    expected_sha: &str,
    on_progress: &Channel<DownloadProgress>,
    cancel: &CancellationToken,
    done: &mut u64,
    total: u64,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("הורדת מודל המגדר נכשלה (קוד {})", resp.status()));
    }
    let mut file = tokio::fs::File::create(dest_tmp)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut hasher = Sha256::new();
    let mut last_emit = Instant::now();
    let mut speed_bytes = 0u64;
    let mut speed_start = Instant::now();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                drop(file);
                let _ = tokio::fs::remove_file(dest_tmp).await;
                return Err("ההורדה בוטלה".into());
            }
            chunk = stream.next() => match chunk {
                None => break,
                Some(Err(e)) => return Err(e.to_string()),
                Some(Ok(bytes)) => {
                    file.write_all(&bytes).await.map_err(|e| e.to_string())?;
                    hasher.update(&bytes);
                    *done += bytes.len() as u64;
                    speed_bytes += bytes.len() as u64;
                    if last_emit.elapsed().as_millis() >= 100 {
                        let el = speed_start.elapsed().as_secs_f64();
                        let speed = if el > 0.0 { (speed_bytes as f64 / el) as u64 } else { 0 };
                        let _ = on_progress.send(DownloadProgress {
                            id: DOWNLOAD_KEY.into(),
                            downloaded_bytes: *done,
                            total_bytes: total,
                            speed_bps: speed,
                        });
                        last_emit = Instant::now();
                        speed_bytes = 0;
                        speed_start = Instant::now();
                    }
                }
            }
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    let got = format!("{:x}", hasher.finalize());
    if !expected_sha.is_empty() && got != expected_sha {
        let _ = tokio::fs::remove_file(dest_tmp).await;
        return Err("סיכום ביקורת (SHA-256) של מודל המגדר אינו תואם — הקובץ פגום".into());
    }
    Ok(())
}
