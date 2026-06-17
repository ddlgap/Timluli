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

/// How to fold the source's audio channels to mono. `DialogueCenter` emphasizes the
/// front-center channel (where film/TV dialogue is mixed) and drops the surround/LFE
/// channels carrying music/effects — a cheap, ffmpeg-only stand-in for the neural
/// dialogue-stem isolation media-transcription pipelines use (e.g. Demucs in
/// whisper-diarization). Cleaner, ~10 dB louder speech → better WER and gender.
/// Applied ONLY when the source actually has a front-center channel (5.1/7.1/…);
/// stereo/mono fall back to `Plain` so behavior there is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Downmix {
    Plain,
    DialogueCenter,
}

/// Front-center-dominant mono mix: mostly FC (dialogue), a little L/R for dialogue
/// that strays off-center, no surrounds/LFE (music/effects/boom).
const DIALOGUE_PAN: &str = "pan=mono|c0=0.6*FC+0.3*FL+0.3*FR";

/// Probes the source's audio channel layout and picks a downmix. A layout exposing a
/// front-center channel → `DialogueCenter`; otherwise (stereo/mono/unreadable) →
/// `Plain`. Conservative on purpose: emitting the FC pan for a source without an FC
/// channel would make ffmpeg error, so only known-FC layouts trigger it.
pub fn probe_downmix(ffmpeg: &Path, input: &Path) -> Downmix {
    let output = quiet(Command::new(ffmpeg))
        .arg("-i")
        .arg(input)
        .arg("-hide_banner")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    match output {
        Ok(o) if has_front_center(&String::from_utf8_lossy(&o.stderr)) => Downmix::DialogueCenter,
        _ => Downmix::Plain,
    }
}

/// True if an `Audio:` line in the ffmpeg banner names a channel layout that includes
/// a front-center (FC) channel (3.0/3.1/5.0/5.1/6.1/7.1, incl. `5.1(side)`). Matches
/// the layout as a whole comma-delimited token (after stripping a `(side)`-style
/// suffix) so it can't false-match a bitrate/sample-rate figure. Excludes layouts
/// without FC: mono, stereo, 2.1, quad/4.0.
fn has_front_center(stderr: &str) -> bool {
    const FC_LAYOUTS: &[&str] = &["3.0", "3.1", "5.0", "5.1", "6.1", "7.1"];
    stderr
        .lines()
        .filter(|l| l.contains("Audio:"))
        .any(|l| {
            l.split(',').any(|tok| {
                let base = tok.trim().split('(').next().unwrap_or("").trim();
                FC_LAYOUTS.contains(&base)
            })
        })
}

/// Extracts 16 kHz mono f32-LE PCM (headerless) for the local engine. `dm` selects the
/// channel→mono fold: `Plain` (standard `-ac 1`, also folds dubbed multi-track) or
/// `DialogueCenter` (front-center dialogue emphasis for multichannel sources).
pub fn extract_pcm_f32le(ffmpeg: &Path, input: &Path, out: &Path, dm: Downmix) -> Result<(), String> {
    let input_s = input.to_string_lossy();
    let out_s = out.to_string_lossy();
    let (input, out): (&str, &str) = (&input_s, &out_s);
    let mut args: Vec<&str> = vec!["-i", input, "-vn", "-ar", "16000"];
    match dm {
        Downmix::Plain => args.extend(["-ac", "1"]),
        Downmix::DialogueCenter => args.extend(["-af", DIALOGUE_PAN]),
    }
    args.extend([
        "-f", "f32le", "-acodec", "pcm_f32le", "-y", "-hide_banner", "-loglevel", "error", out,
    ]);
    run_extract(ffmpeg, &args, Path::new(out))
}

/// Bitrate for the Groq cloud engine's MP3 uploads. 64 kbps mono is transparent for
/// 16 kHz speech (Whisper is robust to lossy compression) while making the file
/// ~5× smaller than FLAC and, crucially, a *predictable* 8 kB/s — so a slice's size
/// is bounded by its duration alone (the basis of the 25 MB upload guarantee).
const MP3_BITRATE: &str = "64k";

/// Extracts 16 kHz mono MP3 for the Groq cloud engine. Compact + fast to upload;
/// at 64 kbps the whole file stays ≈8 kB/s, so videos up to ~50 min fit Groq's
/// 25 MB cap in a single request (longer ones are sliced — see `extract_mp3_range`).
pub fn extract_mp3(ffmpeg: &Path, input: &Path, out: &Path, dm: Downmix) -> Result<(), String> {
    let input_s = input.to_string_lossy();
    let out_s = out.to_string_lossy();
    let (input, out): (&str, &str) = (&input_s, &out_s);
    let mut args: Vec<&str> = vec!["-i", input, "-vn", "-ar", "16000"];
    match dm {
        Downmix::Plain => args.extend(["-ac", "1", "-map", "0:a"]),
        Downmix::DialogueCenter => args.extend(["-af", DIALOGUE_PAN]),
    }
    args.extend([
        "-c:a", "libmp3lame", "-b:a", MP3_BITRATE, "-y", "-hide_banner", "-loglevel", "error", out,
    ]);
    run_extract(ffmpeg, &args, Path::new(out))
}

/// Extracts a time-bounded 16 kHz mono MP3 slice `[start_secs, start_secs+dur_secs)`
/// for the cloud engine's parallel chunked path. At 64 kbps a 5 min slice is ≈2.4 MB,
/// far under Groq's 25 MB cap, and small enough to transcribe many in parallel.
/// `-ss` before `-i` is a fast input seek; audio seek lands sample-accurately enough.
pub fn extract_mp3_range(
    ffmpeg: &Path,
    input: &Path,
    start_secs: f64,
    dur_secs: f64,
    out: &Path,
    dm: Downmix,
) -> Result<(), String> {
    let input_s = input.to_string_lossy();
    let out_s = out.to_string_lossy();
    let (input, out): (&str, &str) = (&input_s, &out_s);
    let (start, dur) = (format!("{start_secs}"), format!("{dur_secs}"));
    let mut args: Vec<&str> = vec!["-ss", &start, "-i", input, "-t", &dur, "-vn", "-ar", "16000"];
    match dm {
        Downmix::Plain => args.extend(["-ac", "1", "-map", "0:a"]),
        Downmix::DialogueCenter => args.extend(["-af", DIALOGUE_PAN]),
    }
    args.extend([
        "-c:a", "libmp3lame", "-b:a", MP3_BITRATE, "-y", "-hide_banner", "-loglevel", "error", out,
    ]);
    run_extract(ffmpeg, &args, Path::new(out))
}

/// Probes the media duration in seconds by parsing ffmpeg's stderr banner (the
/// GPL "essentials" build ships no ffprobe). `ffmpeg -i <file>` with no output exits
/// non-zero but still prints `Duration: HH:MM:SS.ss` — so we read stderr, not status.
pub fn probe_duration(ffmpeg: &Path, input: &Path) -> Result<f64, String> {
    let output = quiet(Command::new(ffmpeg))
        .arg("-i")
        .arg(input)
        .arg("-hide_banner")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("שגיאה בהפעלת ffmpeg: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_duration_secs(&stderr).ok_or_else(|| "לא ניתן לקרוא את משך הסרטון".to_string())
}

/// Parses the `Duration: HH:MM:SS.ss` field out of an ffmpeg stderr banner into
/// seconds. Returns `None` for a missing/`N/A`/malformed value.
fn parse_duration_secs(stderr: &str) -> Option<f64> {
    let idx = stderr.find("Duration:")?;
    let ts = stderr[idx + "Duration:".len()..].split(',').next()?.trim();
    let mut parts = ts.split(':');
    let h: f64 = parts.next()?.trim().parse().ok()?;
    let m: f64 = parts.next()?.trim().parse().ok()?;
    let s: f64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Detects silence intervals (seconds) in the source audio via ffmpeg's
/// `silencedetect` filter — one analysis pass that decodes audio to a null sink
/// and logs `silence_start`/`silence_end` to stderr (the same stderr-scraping
/// approach as [`probe_duration`]; the essentials build ships no ffprobe). Returns
/// `(start_secs, end_secs)` pairs; ANY failure yields an empty list so the caller
/// falls back to fixed-boundary chunking (never worse than before).
///
/// `noise=-30dB` is the RMS threshold below which audio counts as silence —
/// permissive enough to catch real speech pauses without splitting quiet speech;
/// `d=0.25` requires a 0.25 s minimum so only genuine gaps qualify. `-loglevel` is
/// deliberately left at the default `info` (silencedetect logs there); only
/// `-nostats` is set, to keep the stderr we parse small.
pub fn detect_silences(ffmpeg: &Path, input: &Path) -> Vec<(f64, f64)> {
    let output = quiet(Command::new(ffmpeg))
        .args(["-hide_banner", "-nostats"])
        .arg("-i")
        .arg(input)
        .args(["-vn", "-af", "silencedetect=noise=-30dB:d=0.25", "-f", "null", "-"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    match output {
        Ok(o) => parse_silences(&String::from_utf8_lossy(&o.stderr)),
        Err(e) => {
            log::warn!("silencedetect pass failed ({e}); using fixed slice boundaries");
            Vec::new()
        }
    }
}

/// Parses `silence_start:`/`silence_end:` pairs out of an ffmpeg `silencedetect`
/// stderr stream into `(start_secs, end_secs)` intervals. An `end` without a prior
/// `start`, or a non-increasing pair, is skipped.
fn parse_silences(stderr: &str) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut cur_start: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(idx) = line.find("silence_start:") {
            cur_start = leading_f64(&line[idx + "silence_start:".len()..]);
        } else if let Some(idx) = line.find("silence_end:") {
            if let (Some(start), Some(end)) =
                (cur_start.take(), leading_f64(&line[idx + "silence_end:".len()..]))
            {
                let start = start.max(0.0);
                if end > start {
                    out.push((start, end));
                }
            }
        }
    }
    out
}

/// Parses the leading floating-point number from `s` (after trimming leading
/// whitespace), stopping at the first character that cannot be part of a number.
fn leading_f64(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(s.len());
    s[..end].parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::{has_front_center, parse_duration_secs, parse_silences};

    #[test]
    fn detects_front_center_layouts_only() {
        let fc = |layout: &str| {
            has_front_center(&format!(
                "  Stream #0:1(und): Audio: aac (LC), 48000 Hz, {layout}, fltp, 384 kb/s"
            ))
        };
        // Layouts WITH a front-center channel → true.
        for yes in ["5.1", "5.1(side)", "7.1", "5.0", "3.0", "3.1", "6.1"] {
            assert!(fc(yes), "{yes} should be detected as having FC");
        }
        // Layouts WITHOUT a front-center channel → false.
        for no in ["mono", "stereo", "2.1", "quad", "4.0"] {
            assert!(!fc(no), "{no} must NOT be treated as having FC");
        }
        // No Audio line at all → false.
        assert!(!has_front_center("  Stream #0:0: Video: hevc, 1920x1080"));
    }

    #[test]
    fn parses_ffmpeg_duration_banner() {
        let s = "  Duration: 02:08:44.74, start: 0.000000, bitrate: 2391 kb/s";
        assert_eq!(parse_duration_secs(s), Some(2.0 * 3600.0 + 8.0 * 60.0 + 44.74));
        assert_eq!(parse_duration_secs("  Duration: 00:00:26.98, start: 0"), Some(26.98));
    }

    #[test]
    fn rejects_missing_or_na_duration() {
        assert_eq!(parse_duration_secs("no banner here"), None);
        assert_eq!(parse_duration_secs("  Duration: N/A, start: 0"), None);
    }

    #[test]
    fn parses_silencedetect_intervals() {
        let stderr = "[silencedetect @ 0x1] silence_start: 1.5\n\
                      [silencedetect @ 0x1] silence_end: 2.75 | silence_duration: 1.25\n\
                      frame=  100 fps=0.0 q=-0.0 size=N/A time=00:00:05.00\n\
                      [silencedetect @ 0x1] silence_start: 10\n\
                      [silencedetect @ 0x1] silence_end: 11.2 | silence_duration: 1.2\n";
        assert_eq!(parse_silences(stderr), vec![(1.5, 2.75), (10.0, 11.2)]);
    }

    #[test]
    fn silence_end_without_start_or_garbage_is_empty() {
        assert!(parse_silences("nothing to see here").is_empty());
        assert!(parse_silences("[silencedetect] silence_end: 5.0 | silence_duration: 1.0").is_empty());
    }
}

/// Reads a headerless 16 kHz mono f32-LE PCM file into samples.
pub fn read_pcm_f32le(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("שגיאה בקריאת האודיו המחולץ: {e}"))?;
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}
