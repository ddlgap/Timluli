use crate::models::types::*;
use crate::models::{registry, storage};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read as StdRead;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

const META_FILENAME: &str = "meta.json";

/// Scans models_dir and returns all installed models (reads each meta.json).
pub fn list_installed(app: &AppHandle) -> Vec<InstalledModel> {
    let base = storage::models_dir(app);
    let mut installed = Vec::new();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return installed;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Skip temp download dirs
        if path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with(".tmp-"))
            .unwrap_or(false)
        {
            continue;
        }
        let meta_path = path.join(META_FILENAME);
        if let Ok(data) = std::fs::read_to_string(&meta_path) {
            if let Ok(model) = serde_json::from_str::<InstalledModel>(&data) {
                installed.push(model);
            }
        }
    }
    installed
}

/// Recomputes SHA-256 of the model file and compares to meta.json.
pub async fn verify(app: &AppHandle, id: &str) -> bool {
    let dir = storage::model_dir(app, id);
    let meta_path = dir.join(META_FILENAME);
    let Ok(meta_str) = std::fs::read_to_string(&meta_path) else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<InstalledModel>(&meta_str) else {
        return false;
    };
    let file_path = PathBuf::from(&meta.file_path);
    let expected = meta.sha256.clone();

    tokio::task::spawn_blocking(move || hash_file_sha256(&file_path).ok() == Some(expected))
        .await
        .unwrap_or(false)
}

/// Removes the model directory and emits speakly://model-deleted.
pub fn delete(app: &AppHandle, id: &str) -> Result<(), ModelError> {
    let dir = storage::model_dir(app, id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    let _ = app.emit_to(
        "settings",
        "speakly://model-deleted",
        serde_json::json!({ "id": id }),
    );
    Ok(())
}

/// Merges catalog entries with installed status into UI-ready ModelViews.
pub fn merge_view(app: &AppHandle, active_model_id: Option<&str>) -> Vec<ModelView> {
    let catalog = registry::load_catalog(app).unwrap_or_default();
    let installed: HashMap<String, InstalledModel> = list_installed(app)
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();

    let mut views: Vec<ModelView> = catalog
        .iter()
        .map(|entry| {
            let status = match installed.get(&entry.id) {
                Some(inst) if inst.source == "manual" => "manually_imported",
                Some(_) => "installed",
                None => "not_installed",
            }
            .to_string();
            let inst = installed.get(&entry.id);
            ModelView {
                id: entry.id.clone(),
                display_name: entry.display_name.clone(),
                language: entry.language.clone(),
                quality: entry.quality.clone(),
                size_bytes: entry.size_bytes,
                status,
                is_active: active_model_id == Some(entry.id.as_str()),
                source: inst.map(|i| i.source.clone()),
                sha256: inst.map(|i| i.sha256.clone()),
                file_path: inst.map(|i| i.file_path.clone()),
            }
        })
        .collect();

    // Also include manually-imported models that aren't in the catalog.
    for inst in installed.values() {
        if !catalog.iter().any(|e| e.id == inst.id) {
            views.push(ModelView {
                id: inst.id.clone(),
                display_name: inst.display_name.clone(),
                language: "he".into(),
                quality: "unknown".into(),
                size_bytes: 0,
                status: "manually_imported".into(),
                is_active: active_model_id == Some(inst.id.as_str()),
                source: Some(inst.source.clone()),
                sha256: Some(inst.sha256.clone()),
                file_path: Some(inst.file_path.clone()),
            });
        }
    }

    views
}

/// Downloads a model with streaming progress, resume, and SHA-256 verification.
/// On success: atomically installs to model_dir and emits speakly://model-installed.
/// On cancel: deletes the partial temp file.
pub async fn download(
    app: AppHandle,
    id: String,
    on_progress: tauri::ipc::Channel<DownloadProgress>,
    cancel_token: CancellationToken,
) -> Result<(), ModelError> {
    let catalog = registry::load_catalog(&app)?;
    let entry = catalog
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| ModelError::NotFound(id.clone()))?;

    let hf_repo = entry
        .hf_repo_id
        .ok_or_else(|| ModelError::NotFound("hf_repo_id חסר".into()))?;
    let hf_file = entry
        .hf_filename
        .ok_or_else(|| ModelError::NotFound("hf_filename חסר".into()))?;
    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        hf_repo, hf_file
    );
    let expected_sha256 = entry.sha256.clone();
    let display_name = entry.display_name.clone();

    let temp_dir = storage::temp_dir(&app, &id);
    let final_dir = storage::model_dir(&app, &id);

    // Disk space pre-check: refuse if free < size_bytes * 1.2.
    let required = (entry.size_bytes as f64 * 1.2) as u64;
    if let Some(free) = storage::free_space_bytes(&storage::models_dir(&app)) {
        if free < required {
            let free_mb = free / 1_000_000;
            let need_mb = required / 1_000_000;
            return Err(ModelError::Download(format!(
                "שטח דיסק לא מספיק: פנוי {free_mb} MB, נדרש {need_mb} MB"
            )));
        }
    }

    tokio::fs::create_dir_all(&temp_dir).await?;

    let temp_file = temp_dir.join(&hf_file);
    let existing_size = if temp_file.exists() {
        tokio::fs::metadata(&temp_file)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if existing_size > 0 {
        req = req.header("Range", format!("bytes={existing_size}-"));
    }

    let resp = req
        .send()
        .await
        .map_err(|e| ModelError::Download(e.to_string()))?;
    let status = resp.status();

    let (append, start_bytes) = if status == 206 {
        (true, existing_size)
    } else {
        (false, 0u64)
    };
    let total = resp
        .content_length()
        .map(|l| l + start_bytes)
        .unwrap_or(0);

    {
        use tokio::io::AsyncWriteExt;
        let file = if append {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&temp_file)
                .await?
        } else {
            tokio::fs::File::create(&temp_file).await?
        };
        let mut writer = tokio::io::BufWriter::new(file);
        let mut downloaded = start_bytes;
        let mut stream = resp.bytes_stream();
        let mut last_emit = Instant::now();
        let mut speed_bytes = 0u64;
        let mut speed_start = Instant::now();

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    drop(writer);
                    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                    return Err(ModelError::Download("ההורדה בוטלה".into()));
                }
                chunk = stream.next() => {
                    match chunk {
                        None => break,
                        Some(Err(e)) => return Err(ModelError::Download(e.to_string())),
                        Some(Ok(bytes)) => {
                            writer.write_all(&bytes).await?;
                            downloaded += bytes.len() as u64;
                            speed_bytes += bytes.len() as u64;
                            if last_emit.elapsed().as_millis() >= 100 {
                                let elapsed = speed_start.elapsed().as_secs_f64();
                                let speed = if elapsed > 0.0 {
                                    (speed_bytes as f64 / elapsed) as u64
                                } else {
                                    0
                                };
                                let _ = on_progress.send(DownloadProgress {
                                    id: id.clone(),
                                    downloaded_bytes: downloaded,
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
        }
        writer.flush().await?;
    } // writer & file closed here

    // Sanity-check file size before hashing — a few-hundred-byte response means
    // the server returned an error page (e.g. "Entry not found").
    let downloaded_size = tokio::fs::metadata(&temp_file)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if downloaded_size < 1_000_000 {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        return Err(ModelError::Download(format!(
            "הקובץ שהתקבל קטן מדי ({downloaded_size} בייט) — ייתכן שהכתובת שגויה או שהקובץ לא קיים בשרת"
        )));
    }

    // SHA-256 verification (in spawn_blocking — file can be 700MB+).
    let temp_file_path = temp_file.clone();
    let hash =
        tokio::task::spawn_blocking(move || hash_file_sha256(&temp_file_path))
            .await
            .map_err(|e| ModelError::Download(e.to_string()))??;

    if !expected_sha256.starts_with("TBD") && hash != expected_sha256 {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        return Err(ModelError::ChecksumMismatch);
    }

    // Write meta.json and atomically rename temp_dir → final_dir.
    let installed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into());
    let meta = InstalledModel {
        id: id.clone(),
        source: "registry".into(),
        file_path: final_dir.join(&hf_file).to_string_lossy().into_owned(),
        sha256: hash,
        installed_at,
        display_name,
    };
    let meta_json = serde_json::to_string_pretty(&meta)?;
    tokio::fs::write(temp_dir.join(META_FILENAME), meta_json).await?;

    if final_dir.exists() {
        tokio::fs::remove_dir_all(&final_dir).await?;
    }
    tokio::fs::rename(&temp_dir, &final_dir).await?;

    let _ = app.emit_to(
        "settings",
        "speakly://model-installed",
        serde_json::json!({ "id": &id }),
    );
    Ok(())
}

/// Validates magic bytes for ggml/gguf files before import.
pub fn validate_model_magic(path: &Path) -> Result<(), ModelError> {
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    // GGML: bytes on disk spell "ggml" in LE uint32 → {0x6c, 0x6d, 0x67, 0x67}
    // GGUF: literal ASCII "GGUF" → {0x47, 0x47, 0x55, 0x46}
    let ggml = [0x6cu8, 0x6d, 0x67, 0x67];
    let gguf = [0x47u8, 0x47, 0x55, 0x46];
    if magic != ggml && magic != gguf {
        return Err(ModelError::InvalidFormat);
    }
    Ok(())
}

/// Imports a user-supplied model file: validates magic, computes SHA-256, copies
/// into models_dir, and writes meta.json. Returns the new ModelView.
pub async fn import_manual(
    app: AppHandle,
    file_path: String,
    display_name: String,
) -> Result<ModelView, ModelError> {
    let src = PathBuf::from(&file_path);
    if !src.exists() {
        return Err(ModelError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "קובץ לא נמצא",
        )));
    }

    let src_clone = src.clone();
    validate_model_magic(&src_clone)?;

    let id = format!("manual-{}", uuid::Uuid::new_v4());
    let filename = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model.bin".into());

    let dest_dir = storage::model_dir(&app, &id);
    tokio::fs::create_dir_all(&dest_dir).await?;
    let dest_file = dest_dir.join(&filename);

    tokio::fs::copy(&src, &dest_file).await?;

    // Hash in spawn_blocking (slow for large files).
    let dest_clone = dest_file.clone();
    let hash = tokio::task::spawn_blocking(move || hash_file_sha256(&dest_clone))
        .await
        .map_err(|e| ModelError::Download(e.to_string()))??;

    let installed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into());

    let meta = InstalledModel {
        id: id.clone(),
        source: "manual".into(),
        file_path: dest_file.to_string_lossy().into_owned(),
        sha256: hash.clone(),
        installed_at,
        display_name: display_name.clone(),
    };
    let meta_json = serde_json::to_string_pretty(&meta)?;
    tokio::fs::write(dest_dir.join(META_FILENAME), meta_json).await?;

    Ok(ModelView {
        id,
        display_name,
        language: "he".into(),
        quality: "unknown".into(),
        size_bytes: tokio::fs::metadata(&dest_file).await.map(|m| m.len()).unwrap_or(0),
        status: "manually_imported".into(),
        is_active: false,
        source: Some("manual".into()),
        sha256: Some(hash),
        file_path: Some(dest_file.to_string_lossy().into_owned()),
    })
}

// Streaming SHA-256 hash of a file — runs in spawn_blocking.
fn hash_file_sha256(path: &Path) -> Result<String, ModelError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 8 * 1024 * 1024]; // 8 MB chunks
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
