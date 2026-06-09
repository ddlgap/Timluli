//! ffmpeg binary resolution + audio extraction for the video→SRT pipeline.
//!
//! ffmpeg is delivered **on demand** (see `commands_video::download_ffmpeg`), not
//! bundled, so the installer stays lean. It is resolved from
//! `%APPDATA%\studio.oliel.timluli\ffmpeg\ffmpeg.exe` first, then the system PATH
//! (for users who already have it). Extraction shells out exactly like the PDF
//! sidecar (`translation::pdf`): `std::process::Command` + `CREATE_NO_WINDOW`.
//!
//! Two extraction targets, per engine:
//!   - local  → headerless 16 kHz mono **f32-LE** PCM, fed straight to whisper
//!              (no WAV-header/symphonia decode needed — the most direct form).
//!   - cloud  → 16 kHz mono **FLAC** (lossless, ~half the size of WAV for upload).

use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::{AppHandle, Manager};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `%APPDATA%\studio.oliel.timluli\ffmpeg\`.
pub fn ffmpeg_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("ffmpeg")
}

/// The on-demand-downloaded binary location.
pub fn bundled_path(app: &AppHandle) -> PathBuf {
    ffmpeg_dir(app).join("ffmpeg.exe")
}

/// Configures a `Command` to run without flashing a console window (Windows).
fn quiet(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// True if `ffmpeg` resolves on the system PATH (quick `-version` probe).
fn on_path() -> bool {
    quiet(Command::new("ffmpeg"))
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolves an invocable ffmpeg: the downloaded binary if present, else `ffmpeg`
/// from PATH. `None` ⇒ neither available (caller prompts the download).
pub fn resolve(app: &AppHandle) -> Option<PathBuf> {
    let b = bundled_path(app);
    if b.exists() {
        return Some(b);
    }
    if on_path() {
        return Some(PathBuf::from("ffmpeg"));
    }
    None
}

/// Whether extraction is possible right now (downloaded or on PATH).
pub fn is_available(app: &AppHandle) -> bool {
    bundled_path(app).exists() || on_path()
}

/// Runs an extraction command, mapping the outcome to a Hebrew result. `out` must
/// exist and be non-empty on success (a video with no audio track yields neither).
fn run_extract(ffmpeg: &Path, args: &[&str], out: &Path) -> Result<(), String> {
    let status = quiet(Command::new(ffmpeg))
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| format!("שגיאה בהפעלת ffmpeg: {e}"))?;

    if !status.success() {
        let _ = std::fs::remove_file(out);
        return Err("חילוץ האודיו מהווידאו נכשל.".into());
    }
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => {
            let _ = std::fs::remove_file(out);
            Err("לא נמצא פס אודיו בקובץ הווידאו.".into())
        }
    }
}

/// Extracts 16 kHz mono f32-LE PCM (headerless) for the local engine. `-ac 1` also
/// folds dubbed multi-track audio to a single channel.
pub fn extract_pcm_f32le(ffmpeg: &Path, input: &Path, out: &Path) -> Result<(), String> {
    let (input, out) = (input.to_string_lossy(), out.to_string_lossy());
    run_extract(
        ffmpeg,
        &[
            "-i", &input, "-vn", "-ar", "16000", "-ac", "1", "-f", "f32le", "-acodec",
            "pcm_f32le", "-y", "-hide_banner", "-loglevel", "error", &out,
        ],
        Path::new(out.as_ref()),
    )
}

/// Extracts 16 kHz mono FLAC (lossless, compact) for the Groq cloud engine.
pub fn extract_flac(ffmpeg: &Path, input: &Path, out: &Path) -> Result<(), String> {
    let (input, out) = (input.to_string_lossy(), out.to_string_lossy());
    run_extract(
        ffmpeg,
        &[
            "-i", &input, "-vn", "-ar", "16000", "-ac", "1", "-map", "0:a", "-c:a", "flac",
            "-y", "-hide_banner", "-loglevel", "error", &out,
        ],
        Path::new(out.as_ref()),
    )
}

/// Reads a headerless 16 kHz mono f32-LE PCM file into samples.
pub fn read_pcm_f32le(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("שגיאה בקריאת האודיו המחולץ: {e}"))?;
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}
