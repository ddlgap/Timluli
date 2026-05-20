use crate::models::types::{ModelEntry, ModelError};
use serde::Deserialize;
use tauri::{AppHandle, Manager};

#[derive(Deserialize)]
struct ModelCatalog {
    #[allow(dead_code)]
    schema_version: u32,
    models: Vec<ModelEntry>,
}

/// Loads the bundled models.toml catalog. Unknown engine kinds are logged and
/// skipped so future entries don't break older builds.
pub fn load_catalog(app: &AppHandle) -> Result<Vec<ModelEntry>, ModelError> {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| ModelError::Tauri(e.to_string()))?;
    let path = resource_dir.join("resources").join("models.toml");
    let toml_str = std::fs::read_to_string(&path)?;
    let catalog: ModelCatalog =
        toml::from_str(&toml_str).map_err(|e| ModelError::Toml(e.to_string()))?;
    let entries = catalog
        .models
        .into_iter()
        .filter(|m| {
            if m.engine.kind != "whisper-cpp" {
                log::warn!(
                    "מנוע לא מוכר '{}' עבור מודל '{}' — מדולג",
                    m.engine.kind,
                    m.id
                );
                false
            } else {
                true
            }
        })
        .collect();
    Ok(entries)
}
