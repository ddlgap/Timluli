//! Hebrew auto-punctuation commands: status, on-demand model download (reuses the
//! `DownloadProgress` channel shape from the whisper model manager), enable/disable
//! with lazy load/unload, startup autoload, and the `punctuate_if_ready` helper the
//! injection sites call. The model runs in-process (see `src/punctuation/`).

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

use crate::models::types::DownloadProgress;
use crate::punctuation::PunctuationEngineHandle;
use crate::{settings, AppState};

// Artifacts hosted as GitHub release assets on ddlgap/Timluli (matches the existing
// updater-artifact pattern). SHA-256 + sizes are from the verified INT8 build.
const MODEL_URL: &str =
    "https://github.com/ddlgap/Timluli/releases/download/punct-model-v1/punct-model.onnx";
const MODEL_SHA256: &str = "e3a137600b866622af241d101370b7197beffa6aeb2a9c79e5e56040add8ae1f";
const MODEL_SIZE: u64 = 278_908_658;
const TOKENIZER_URL: &str =
    "https://github.com/ddlgap/Timluli/releases/download/punct-model-v1/punct-tokenizer.json";
const TOKENIZER_SHA256: &str = "cfa4f050ec8d6908ea168ce1005eb81630f16b8b37d31b187f90c2c519d389e8";
const TOKENIZER_SIZE: u64 = 17_095_887;

const DOWNLOAD_KEY: &str = "punctuation";

fn punct_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("punctuation")
}
fn model_path(app: &AppHandle) -> PathBuf {
    punct_dir(app).join("model.onnx")
}
fn tokenizer_path(app: &AppHandle) -> PathBuf {
    punct_dir(app).join("tokenizer.json")
}
fn is_installed(app: &AppHandle) -> bool {
    model_path(app).exists() && tokenizer_path(app).exists()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PunctStatus {
    pub installed: bool,
    pub loaded: bool,
    pub enabled: bool,
    pub downloading: bool,
}

#[tauri::command]
pub fn get_punctuation_status(app: AppHandle, state: State<'_, AppState>) -> Result<PunctStatus, String> {
    let enabled = settings::load_or_init(&app)
        .map(|s| s.punctuation_enabled)
        .unwrap_or(false);
    Ok(PunctStatus {
        installed: is_installed(&app),
        loaded: state.punct_engine.lock().is_some(),
        enabled,
        downloading: state.active_downloads.lock().contains_key(DOWNLOAD_KEY),
    })
}

/// Load the model into `AppState` (blocking load on a `spawn_blocking` thread).
async fn load_engine(app: &AppHandle) -> Result<(), String> {
    let mp = model_path(app);
    let tp = tokenizer_path(app);
    if !mp.exists() || !tp.exists() {
        return Err("מודל הפיסוק לא מותקן. הורד אותו תחילה בהגדרות → מנוע תמלול.".into());
    }
    let handle = tokio::task::spawn_blocking(move || PunctuationEngineHandle::load(&mp, &tp))
        .await
        .map_err(|e| format!("שגיאת thread בטעינת מנוע הפיסוק: {e}"))??;
    *app.state::<AppState>().punct_engine.lock() = Some(Arc::new(handle));
    log::info!("punctuation engine loaded");
    Ok(())
}

#[tauri::command]
pub async fn set_punctuation_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut stg = settings::load_or_init(&app)?;
    stg.punctuation_enabled = enabled;
    settings::save(&app, &stg)?;

    if enabled {
        // Lazy-load on first enable (no-op if already loaded). Errors if not installed.
        let loaded = app.state::<AppState>().punct_engine.lock().is_some();
        if !loaded {
            load_engine(&app).await?;
        }
    } else {
        // Free the ~283 MB of model RAM when disabled.
        *app.state::<AppState>().punct_engine.lock() = None;
    }
    let _ = app.emit_to("settings", "speakly://punct-status-changed", enabled);
    Ok(())
}

/// Called from lib.rs setup: if punctuation is enabled and installed, load the
/// model in the background so the first dictation has no warm-up.
pub async fn autoload_punctuation(app: &AppHandle) {
    if app.state::<AppState>().punct_engine.lock().is_some() {
        return;
    }
    let mp = model_path(app);
    let tp = tokenizer_path(app);
    if !mp.exists() || !tp.exists() {
        return;
    }
    match tokio::task::spawn_blocking(move || PunctuationEngineHandle::load(&mp, &tp)).await {
        Ok(Ok(h)) => {
            *app.state::<AppState>().punct_engine.lock() = Some(Arc::new(h));
            log::info!("auto-loaded punctuation engine");
        }
        Ok(Err(e)) => log::error!("punctuation auto-load: {e}"),
        Err(e) => log::error!("punctuation auto-load thread: {e}"),
    }
}

// ─── Download ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn download_punctuation_model(
    app: AppHandle,
    state: State<'_, AppState>,
    on_progress: Channel<DownloadProgress>,
) -> Result<(), String> {
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
                let _ = app_clone.emit_to("settings", "speakly://punct-model-installed", ());
            }
            Err(e) => {
                log::error!("punctuation download: {e}");
                let _ = app_clone.emit_to("settings", "speakly://error", e);
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_punctuation_download(state: State<'_, AppState>) -> Result<(), String> {
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
    let dir = punct_dir(app);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;

    let total = MODEL_SIZE + TOKENIZER_SIZE;
    let mut done: u64 = 0;

    let tok_tmp = dir.join("tokenizer.json.part");
    let model_tmp = dir.join("model.onnx.part");

    // Tokenizer first (small), then the model (the bulk of the progress bar).
    download_one(TOKENIZER_URL, &tok_tmp, TOKENIZER_SHA256, &on_progress, &cancel, &mut done, total).await?;
    download_one(MODEL_URL, &model_tmp, MODEL_SHA256, &on_progress, &cancel, &mut done, total).await?;

    // Final 100% tick, then atomically swap the .part files into place.
    let _ = on_progress.send(DownloadProgress {
        id: DOWNLOAD_KEY.into(),
        downloaded_bytes: total,
        total_bytes: total,
        speed_bps: 0,
    });
    tokio::fs::rename(&tok_tmp, tokenizer_path(app))
        .await
        .map_err(|e| e.to_string())?;
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
        return Err(format!("הורדת מודל הפיסוק נכשלה (קוד {})", resp.status()));
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
        return Err("סיכום ביקורת (SHA-256) של מודל הפיסוק אינו תואם — הקובץ פגום".into());
    }
    Ok(())
}

// ─── Injection-site helper ───────────────────────────────────────────────────────

/// Punctuate `text` if the engine is loaded; otherwise return it unchanged. Used by
/// the local-engine async path. `ensure_terminal` = finalized utterance → guarantee
/// a sentence-ending mark. `newlines` = start a new line after each sentence.
pub async fn punctuate_if_ready(
    state: &AppState,
    text: String,
    ensure_terminal: bool,
    newlines: bool,
) -> String {
    let handle = state.punct_engine.lock().as_ref().map(Arc::clone);
    match handle {
        Some(h) => {
            let out = h.punctuate(text, ensure_terminal).await;
            if newlines {
                to_line_per_sentence(&out)
            } else {
                out
            }
        }
        None => text, // punctuation off → don't reformat
    }
}

/// One sentence per line: replace each sentence-ending mark (`. ? !`) + following
/// spaces with the mark + a newline. Injected as a literal line break via clipboard
/// (see `text_injection`), so it does NOT "send" in chat apps.
pub fn to_line_per_sentence(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if matches!(c, '.' | '?' | '!') {
            while matches!(chars.peek(), Some(' ') | Some('\t')) {
                chars.next();
            }
            out.push('\n');
        }
    }
    out
}
