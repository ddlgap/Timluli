use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MicPosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationMode {
    Toggle,
    PushToTalk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicSize {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub language: String,
    pub shortcut: String,
    pub activation_mode: ActivationMode,
    pub mic_size: MicSize,
    pub mic_opacity: f32,
    #[serde(default = "default_silence_timeout_ms")]
    pub silence_timeout_ms: u32,
    pub start_with_windows: bool,
    pub show_mic_on_startup: bool,
    pub mute_during_fullscreen: bool,
    pub mute_during_calls: bool,
    pub mic_position: Option<MicPosition>,
    #[serde(default = "default_engine_id")]
    pub engine_id: String,
    #[serde(default)]
    pub local_model_id: Option<String>,
    #[serde(default = "default_mic_theme")]
    pub mic_theme: String,
    #[serde(default)]
    pub onboarding_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "he-IL".into(),
            shortcut: "Ctrl+Super+Space".into(),
            activation_mode: ActivationMode::Toggle,
            mic_size: MicSize::Medium,
            mic_opacity: 0.95,
            silence_timeout_ms: 1500,
            start_with_windows: false,
            show_mic_on_startup: true,
            mute_during_fullscreen: false,
            mute_during_calls: false,
            mic_position: None,
            engine_id: default_engine_id(),
            local_model_id: None,
            mic_theme: default_mic_theme(),
            onboarding_done: false,
        }
    }
}

fn default_silence_timeout_ms() -> u32 { 1500 }
fn default_engine_id() -> String { "web-speech".into() }
fn default_mic_theme() -> String { "graphite".into() }

fn settings_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .or_else(|_| app.path().app_data_dir())
        .unwrap_or_else(|_| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Timluli")
        })
}

fn settings_path(app: &AppHandle) -> PathBuf {
    settings_dir(app).join("settings.json")
}

pub fn load_or_init(app: &AppHandle) -> Result<Settings, String> {
    let path = settings_path(app);
    if !path.exists() {
        let dir = settings_dir(app);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let defaults = Settings::default();
        let json = serde_json::to_string_pretty(&defaults).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())?;
        return Ok(defaults);
    }
    let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).or_else(|_| {
        // Self-heal: corrupt file → reset to defaults.
        let defaults = Settings::default();
        let _ = fs::write(&path, serde_json::to_string_pretty(&defaults).unwrap_or_default());
        Ok(defaults)
    })
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app);
    let dir = settings_dir(app);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}
