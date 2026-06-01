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
    #[serde(default = "default_translate_target_language")]
    pub translate_target_language: String,
    /// PDF→PDF Hebrew RTL layout mode passed to the `timluli-pdf` sidecar.
    /// `"same-box"` (default) keeps each block in its original position; `"mirror-text"`
    /// mirrors safe text blocks to the right within the page content frame.
    #[serde(default = "default_pdf_rtl_layout")]
    pub pdf_rtl_layout: String,
    /// Experimental: when true, the mic window auto-docks next to the focused
    /// text field via UI Automation. Off by default while we gather coverage data.
    #[serde(default)]
    pub field_docking_enabled: bool,
    /// Preferred Groq chat model for document translation. `None`/empty = automatic
    /// (use the built-in fallback chain). When set, it is tried first, then the
    /// remaining chain serves as backup.
    #[serde(default)]
    pub groq_model: Option<String>,
    /// Preferred Cerebras chat model for document translation. See `groq_model`.
    #[serde(default)]
    pub cerebras_model: Option<String>,
    /// When true, treat the Groq key as a paid/Developer-tier key: translate in
    /// parallel (concurrent batches, no fixed inter-batch sleep) to exploit the
    /// much higher paid rate limits. Off = conservative free-tier behavior.
    #[serde(default)]
    pub groq_paid: bool,
    /// When true, treat the Cerebras key as a paid-tier key. See `groq_paid`.
    #[serde(default)]
    pub cerebras_paid: bool,
    /// Backend used to transcribe audio files dragged onto the mic.
    /// `"groq"` (cloud, whisper-large-v3-turbo, reuses the Groq key) or
    /// `"whisper-local"` (offline, the loaded local model). Independent of
    /// `engine_id`, which only governs live dictation.
    #[serde(default = "default_audio_file_engine")]
    pub audio_file_engine: String,
    /// UI display mode: `"floating-mic"` (default, the draggable mic button) or
    /// `"side-panel"` (a vertical handle docked to the right screen edge that
    /// expands into a 3-button panel). Mutually exclusive — only one window is
    /// shown at a time.
    #[serde(default = "default_display_mode")]
    pub display_mode: String,
    /// Physical Y of the side-panel's top edge (X is always anchored to the
    /// right work-area edge). `None` = vertically centered. Analogous to
    /// `mic_position` but only the vertical axis is user-controlled.
    #[serde(default)]
    pub panel_offset_y: Option<i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "he-IL".into(),
            shortcut: "Ctrl+Ctrl".into(),
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
            translate_target_language: default_translate_target_language(),
            pdf_rtl_layout: default_pdf_rtl_layout(),
            field_docking_enabled: false,
            groq_model: None,
            cerebras_model: None,
            groq_paid: false,
            cerebras_paid: false,
            audio_file_engine: default_audio_file_engine(),
            display_mode: default_display_mode(),
            panel_offset_y: None,
        }
    }
}

fn default_silence_timeout_ms() -> u32 { 1500 }
fn default_engine_id() -> String { "web-speech".into() }
fn default_mic_theme() -> String { "graphite".into() }
fn default_translate_target_language() -> String { "Hebrew".into() }
fn default_pdf_rtl_layout() -> String { "same-box".into() }
fn default_audio_file_engine() -> String { "groq".into() }
fn default_display_mode() -> String { "floating-mic".into() }

pub fn settings_dir(app: &AppHandle) -> PathBuf {
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
