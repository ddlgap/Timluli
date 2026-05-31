//! Audio-file transcription pipeline: drag an audio file onto the mic and get a
//! plain-text transcript saved next to the source. Distinct from live dictation
//! (`commands_local::transcribe_local`) and from document translation
//! (`translation/`).
//!
//! Two selectable backends (`settings.audio_file_engine`):
//!   - `"groq"`         → cloud STT via whisper-large-v3-turbo (reuses the Groq key)
//!   - `"whisper-local"`→ the locally loaded ivrit.ai model
//!
//! Output is written as `<stem>.txt` (UTF-8) beside the input; the original is
//! untouched. All user-facing errors are Hebrew, matching the rest of the app.

mod groq;
mod local;

use crate::AppState;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};

/// Builds the `<stem>.txt` output path next to the source file.
fn output_txt_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("transcript");
    input.with_file_name(format!("{stem}.txt"))
}

/// Entry point: validates the file, dispatches to the configured backend, and
/// writes the transcript next to the source. Returns the output file path.
pub async fn transcribe_audio_file(
    app: &AppHandle,
    state: State<'_, AppState>,
    path: &str,
) -> Result<String, String> {
    let input = PathBuf::from(path);
    if !input.exists() {
        return Err(format!("הקובץ לא נמצא: {path}"));
    }

    let backend = crate::settings::load_or_init(app)?.audio_file_engine;

    let text = match backend.as_str() {
        "whisper-local" => local::transcribe(app, state, &input).await?,
        // Default to the cloud backend for any unknown value.
        _ => groq::transcribe(app, &input).await?,
    };

    let text = text.trim();
    if text.is_empty() {
        return Err("לא זוהה דיבור בקובץ האודיו".into());
    }

    let out = output_txt_path(&input);
    std::fs::write(&out, text).map_err(|e| format!("שגיאה בכתיבת הפלט: {e}"))?;
    Ok(out.to_string_lossy().into_owned())
}
