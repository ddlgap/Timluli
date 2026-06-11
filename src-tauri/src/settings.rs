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
    /// text field via UI Automation. Defaults ON for new installs (see the `Default`
    /// impl). `#[serde(default)]` still yields `false` for older settings.json files
    /// missing the field, so users who installed before it existed stay untouched.
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
    /// UI display mode: `"side-panel"` (default — a vertical handle docked to the
    /// right screen edge that expands into a 3-button panel) or `"floating-mic"`
    /// (the draggable mic button). Mutually exclusive — only one window is
    /// shown at a time.
    #[serde(default = "default_display_mode")]
    pub display_mode: String,
    /// Physical Y of the side-panel's top edge (X is always anchored to the
    /// right work-area edge). `None` = vertically centered. Analogous to
    /// `mic_position` but only the vertical axis is user-controlled.
    #[serde(default)]
    pub panel_offset_y: Option<i32>,
    /// Migration sentinel: set once the saved `mic_position` has been adjusted
    /// for the enlarged (160→240) mic window. Prevents the one-time shift from
    /// being applied twice. See `migrate_mic_window_v2`.
    #[serde(default)]
    pub mic_window_v2: bool,
    /// Hebrew auto-punctuation: when on (and the model is downloaded + loaded),
    /// final transcripts get `. , ?` restored before injection. Opt-in, default
    /// off (requires a one-time ~283 MB model download). See `src/punctuation/`.
    #[serde(default)]
    pub punctuation_enabled: bool,
    /// When punctuation is on, start a new line after every sentence-ending mark
    /// (`. ? !`). Injected as a literal line break via clipboard paste, so it does
    /// not send in chat apps. Opt-in, default off.
    #[serde(default)]
    pub punctuation_newline: bool,
    /// Video → SRT subtitles: when on, dragging a video file produces a
    /// `<stem>.he.srt` next to it (via ffmpeg + the chosen STT engine) instead of
    /// the plain `.txt` audio path. Default on; `default_true` so existing
    /// settings.json files get it too. Off = today's behavior exactly (videos fall
    /// through to the audio→txt path). Requires a one-time ffmpeg download.
    #[serde(default = "default_true")]
    pub video_subtitles_enabled: bool,
    /// Subtitle burn-in style preset: `"classic" | "box" | "fade" | "pop" |
    /// "karaoke"`. Applied when a video + SRT pair is dropped on the mic/panel.
    /// Unknown values fall back to classic at burn time (`ass::Preset::from_id`).
    #[serde(default = "default_burn_style")]
    pub burn_style: String,
    /// Experimental: acoustic speaker-gender detection (per-cue F0) feeding
    /// `[M]`/`[F]` tags into Hebrew subtitle translation for correct gender
    /// inflections. Off by default — it changes translation output. See
    /// `src/gender_f0.rs`.
    #[serde(default)]
    pub gender_aware_translation: bool,
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
            field_docking_enabled: true,
            groq_model: None,
            cerebras_model: None,
            groq_paid: false,
            cerebras_paid: false,
            audio_file_engine: default_audio_file_engine(),
            display_mode: default_display_mode(),
            panel_offset_y: None,
            mic_window_v2: false,
            punctuation_enabled: false,
            punctuation_newline: false,
            video_subtitles_enabled: true,
            burn_style: default_burn_style(),
            gender_aware_translation: false,
        }
    }
}

fn default_silence_timeout_ms() -> u32 { 1500 }
fn default_engine_id() -> String { "web-speech".into() }
fn default_mic_theme() -> String { "graphite".into() }
fn default_translate_target_language() -> String { "Hebrew".into() }
fn default_pdf_rtl_layout() -> String { "same-box".into() }
fn default_audio_file_engine() -> String { "groq".into() }
fn default_display_mode() -> String { "side-panel".into() }
fn default_true() -> bool { true }
fn default_burn_style() -> String { "classic".into() }

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

/// One-time migration for the enlarged mic window (160→240 logical px). The
/// saved `mic_position` is the window's **physical** top-left; because the mic
/// button is centered in its window, a bigger window shifts the *visible* mic by
/// half the size delta. Subtract that half-delta (40 logical px → physical via
/// `scale`) from both axes so the floating mic stays exactly where the user left
/// it after upgrading. Idempotent: guarded by `mic_window_v2`, persisted once.
pub fn migrate_mic_window_v2(app: &AppHandle, settings: &mut Settings, scale: f64) {
    if settings.mic_window_v2 {
        return;
    }
    if let Some(pos) = settings.mic_position.as_mut() {
        let d = (40.0 * scale).round() as i32;
        pos.x -= d;
        pos.y -= d;
    }
    settings.mic_window_v2 = true;
    let _ = save(app, settings);
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app);
    let dir = settings_dir(app);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}
