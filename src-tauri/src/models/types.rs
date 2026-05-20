use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub language: String,
    pub quality: String,
    pub size_bytes: u64,
    pub license: String,
    pub source: String,
    pub hf_repo_id: Option<String>,
    pub hf_filename: Option<String>,
    pub sha256: String,
    pub engine: EngineSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSpec {
    pub kind: String,
    pub expected_format: String,
}

/// Runtime record written to meta.json inside each installed model directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModel {
    pub id: String,
    /// "registry" (downloaded from catalog) | "manual" (user-imported file)
    pub source: String,
    pub file_path: String,
    pub sha256: String,
    pub installed_at: String,
    pub display_name: String,
}

/// Flat shape sent to the frontend via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    pub id: String,
    pub display_name: String,
    pub language: String,
    pub quality: String,
    pub size_bytes: u64,
    pub status: String,
    pub is_active: bool,
    pub source: Option<String>,
    pub sha256: Option<String>,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub id: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub id: String,
    pub display_name: String,
    pub ready: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("מודל לא נמצא: {0}")]
    NotFound(String),
    #[error("שגיאת IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("שגיאת JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("שגיאת TOML: {0}")]
    Toml(String),
    #[error("סיכום ביקורת (SHA-256) אינו תואם — הקובץ פגום")]
    ChecksumMismatch,
    #[error("פורמט קובץ מודל לא תקין (נדרש ggml/gguf)")]
    InvalidFormat,
    #[error("שגיאת הורדה: {0}")]
    Download(String),
    #[error("שגיאת Tauri: {0}")]
    Tauri(String),
}
