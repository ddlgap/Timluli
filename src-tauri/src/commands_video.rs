//! Video→SRT commands: ffmpeg status, on-demand ffmpeg download (reuses the
//! `DownloadProgress` channel + `active_downloads` cancellation pattern from
//! `commands_punct`), and the `transcribe_video_to_srt` orchestrator wrapper.
//! Logic lives in `src/video_transcription/`.

use std::time::Instant;

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::models::types::DownloadProgress;
use crate::video_transcription::{self, ffmpeg};
use crate::AppState;

// Self-hosted GitHub release asset on ddlgap/Timluli — matches the punct-model and
// CI-downloaded onnxruntime pattern (immutable URL, our control, SHA-verified).
//
// The asset is the GPL "essentials" ffmpeg.exe (win64) published on the
// `ffmpeg-v1` prerelease (ffmpeg 8.1.1-essentials from gyan.dev). An empty SHA
// would skip verification (used during local testing); the real digest is set
// below so a tampered/corrupt download is rejected. The progress-bar total comes
// from the response's Content-Length, so no size constant is needed.
const FFMPEG_URL: &str = "https://github.com/ddlgap/Timluli/releases/download/ffmpeg-v1/ffmpeg.exe";
const FFMPEG_SHA256: &str = "228d7a8556258de907fdb55f36850078ebc7680b84ec30d84ea02e99bec1d1eb";

const DOWNLOAD_KEY: &str = "ffmpeg";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegStatus {
    /// True if ffmpeg can be invoked now (downloaded into AppData or on PATH).
    pub installed: bool,
    pub downloading: bool,
}

#[tauri::command]
pub fn get_ffmpeg_status(app: AppHandle, state: State<'_, AppState>) -> Result<FfmpegStatus, String> {
    Ok(FfmpegStatus {
        installed: ffmpeg::is_available(&app),
        downloading: state.active_downloads.lock().contains_key(DOWNLOAD_KEY),
    })
}

#[tauri::command]
pub async fn download_ffmpeg(
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
                let _ = app_clone.emit_to("settings", "speakly://ffmpeg-installed", ());
            }
            Err(e) => {
                log::error!("ffmpeg download: {e}");
                let _ = app_clone.emit_to("settings", "speakly://error", e);
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_ffmpeg_download(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(token) = state.active_downloads.lock().remove(DOWNLOAD_KEY) {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn transcribe_video_to_srt(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    let result = video_transcription::transcribe_video_to_srt(&app, state, &path).await;
    match &result {
        Ok(out) => {
            let _ = app.emit_to("mic", "speakly://transcribe-done", out.clone());
            let _ = app.emit_to("panel", "speakly://transcribe-done", out.clone());
        }
        Err(e) => {
            let _ = app.emit_to("mic", "speakly://transcribe-error", e.clone());
            let _ = app.emit_to("panel", "speakly://transcribe-error", e.clone());
        }
    }
    result
}

/// Streams the single ffmpeg.exe asset to `ffmpeg.exe.part`, verifies SHA-256 (when
/// configured), then atomically renames it into place. Mirrors
/// `commands_punct::download_one`, specialized for one file with a Content-Length
/// progress total.
async fn do_download(
    app: &AppHandle,
    on_progress: Channel<DownloadProgress>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let dir = ffmpeg::ffmpeg_dir(app);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let tmp = dir.join("ffmpeg.exe.part");
    let dest = ffmpeg::bundled_path(app);

    let client = reqwest::Client::new();
    let resp = client
        .get(FFMPEG_URL)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("הורדת ffmpeg נכשלה (קוד {})", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);

    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut last_emit = Instant::now();
    let mut speed_bytes = 0u64;
    let mut speed_start = Instant::now();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err("ההורדה בוטלה".into());
            }
            chunk = stream.next() => match chunk {
                None => break,
                Some(Err(e)) => return Err(e.to_string()),
                Some(Ok(bytes)) => {
                    file.write_all(&bytes).await.map_err(|e| e.to_string())?;
                    hasher.update(&bytes);
                    done += bytes.len() as u64;
                    speed_bytes += bytes.len() as u64;
                    if last_emit.elapsed().as_millis() >= 100 {
                        let el = speed_start.elapsed().as_secs_f64();
                        let speed = if el > 0.0 { (speed_bytes as f64 / el) as u64 } else { 0 };
                        let _ = on_progress.send(DownloadProgress {
                            id: DOWNLOAD_KEY.into(),
                            downloaded_bytes: done,
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
    if !FFMPEG_SHA256.is_empty() && got != FFMPEG_SHA256 {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err("סיכום ביקורת (SHA-256) של ffmpeg אינו תואם — הקובץ פגום".into());
    }

    let final_total = total.max(done);
    let _ = on_progress.send(DownloadProgress {
        id: DOWNLOAD_KEY.into(),
        downloaded_bytes: final_total,
        total_bytes: final_total,
        speed_bps: 0,
    });
    tokio::fs::rename(&tmp, &dest)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
