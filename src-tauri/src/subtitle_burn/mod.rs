//! Subtitle burn-in: video + SRT → new video with hardcoded styled subtitles.
//! See the spec at `.claude/temp/SPEC_subtitle_burning.md` and the Phase 0
//! findings at `.claude/temp/PHASE0_subtitle_burning.md`.
//!
//! Flow: parse SRT → build a styled ASS (`ass.rs`, preset from
//! `settings.burn_style`) → write it to an ASCII temp filename → run ffmpeg's
//! `ass` filter with `current_dir` set to the temp dir, so the filter argument
//! is a bare relative filename and needs **zero escaping** (the `ass=` filter
//! arg parser chokes on `C:\`, quotes and non-ASCII; the video paths themselves
//! are ordinary `-i`/output args and may contain Hebrew/spaces freely).
//!
//! Encoding is real work (libx264 re-encode; audio is stream-copied), so the
//! blocking subprocess runs on `spawn_blocking` and reports percent progress
//! parsed from `-progress pipe:1` against a duration probed up front.

pub mod ass;
pub mod srt_parse;

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::video_transcription::{ffmpeg, words};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn quiet(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Emits a burn progress tick (0-100) to both drop surfaces.
fn emit_progress(app: &AppHandle, percent: u8) {
    let payload = json!({ "percent": percent });
    let _ = app.emit_to("mic", "speakly://burn-progress", payload.clone());
    let _ = app.emit_to("panel", "speakly://burn-progress", payload);
}

/// Entry point result. Returned to the invoking webview (serialized) AND echoed
/// in the done event. `degraded` is true when the requested style could not be
/// honored — karaoke without (usable) word timings renders as box instead.
#[derive(Serialize, Clone)]
pub struct BurnOutcome {
    pub output_path: String,
    pub style_used: String,
    pub degraded: bool,
}

pub async fn burn_subtitles(
    app: &AppHandle,
    video_path: &str,
    srt_path: &str,
) -> Result<BurnOutcome, String> {
    let video = PathBuf::from(video_path);
    let srt = PathBuf::from(srt_path);
    if !video.exists() {
        return Err(format!("קובץ הווידאו לא נמצא: {video_path}"));
    }
    if !srt.exists() {
        return Err(format!("קובץ הכתוביות לא נמצא: {srt_path}"));
    }

    let ffmpeg_bin = ffmpeg::resolve(app).ok_or_else(|| {
        "לצריבת כתוביות יש להתקין את ffmpeg. הורד אותו בהגדרות → מנוע תמלול.".to_string()
    })?;

    // SRT must be UTF-8 (Timluli's own output always is). A clear error beats a
    // silently garbled burn of a Windows-1255 file.
    let srt_bytes = std::fs::read(&srt).map_err(|e| format!("שגיאה בקריאת קובץ הכתוביות: {e}"))?;
    let srt_text = String::from_utf8(srt_bytes)
        .map_err(|_| "קובץ הכתוביות אינו בקידוד UTF-8. שמור אותו מחדש כ-UTF-8 ונסה שוב.".to_string())?;
    let cues = srt_parse::parse(&srt_text);
    if cues.is_empty() {
        return Err("קובץ הכתוביות ריק או פגום — לא נמצאו כתוביות תקינות.".into());
    }

    let style_id = crate::settings::load_or_init(app)?.burn_style;
    let requested = ass::Preset::from_id(&style_id);
    // Karaoke needs the `<stem>.words.json` sidecar (written by the video→SRT
    // pipeline for Groq/local engines). Missing/invalid sidecar — or one whose
    // words match none of the cues (heavily edited SRT) — degrades the whole
    // burn to the box style. Per-cue mismatches degrade only that cue, inside
    // `build_karaoke`. Degradation, never failure.
    let (ass_doc, style_used, degraded) = match requested {
        ass::Preset::Karaoke => match words::load_for_srt(&srt) {
            Some(timed_words) => {
                let (doc, matched) = ass::build_karaoke(&cues, &timed_words);
                if matched > 0 {
                    (doc, "karaoke".to_string(), false)
                } else {
                    log::warn!("karaoke: words.json present but no cue matched — box fallback");
                    (ass::build(&cues, ass::Preset::Box), "box".to_string(), true)
                }
            }
            None => (ass::build(&cues, ass::Preset::Box), "box".to_string(), true),
        },
        p => (ass::build(&cues, p), style_id, false),
    };
    let tmp_dir = std::env::temp_dir();
    let ass_name = format!("timluli_burn_{}.ass", uuid::Uuid::new_v4());
    let ass_path = tmp_dir.join(&ass_name);
    std::fs::write(&ass_path, &ass_doc).map_err(|e| format!("שגיאה בכתיבת קובץ זמני: {e}"))?;

    let out_path = free_output_path(&video);
    let part_path = out_path.with_extension("mp4.part");

    // The encode is a long blocking subprocess (+ pipe reads) — keep it off the
    // async runtime, like every other ffmpeg call in the codebase.
    let app2 = app.clone();
    let (video2, out2, part2) = (video.clone(), out_path.clone(), part_path.clone());
    let encode = tokio::task::spawn_blocking(move || {
        // Always remove the temp ASS — success or failure.
        let _guard = TempCleanup(ass_path);
        let duration_cs = probe_duration_cs(&ffmpeg_bin, &video2);

        emit_progress(&app2, 0);
        // Try audio stream-copy first (lossless, fast); some sources carry audio
        // codecs the mp4 muxer rejects (e.g. PCM in AVI) — retry once with AAC.
        let mut result = run_burn(&app2, &ffmpeg_bin, &video2, &ass_name, &tmp_dir, &part2, duration_cs, false);
        if result.is_err() {
            emit_progress(&app2, 0);
            result = run_burn(&app2, &ffmpeg_bin, &video2, &ass_name, &tmp_dir, &part2, duration_cs, true);
        }
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part2);
            return Err(e);
        }
        std::fs::rename(&part2, &out2).map_err(|e| {
            let _ = std::fs::remove_file(&part2);
            format!("שגיאה בשמירת קובץ הפלט: {e}")
        })?;
        emit_progress(&app2, 100);
        Ok(())
    });
    encode
        .await
        .map_err(|e| format!("שגיאת thread בצריבה: {e}"))??;

    Ok(BurnOutcome {
        output_path: out_path.to_string_lossy().into_owned(),
        style_used,
        degraded,
    })
}

/// Removes the temp ASS file when the burn ends, however it ends.
struct TempCleanup(PathBuf);
impl Drop for TempCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Runs one ffmpeg encode attempt on a blocking thread, streaming `-progress`
/// key=value lines from stdout into percent events. `transcode_audio` switches
/// `-c:a copy` → AAC for mp4-incompatible source audio.
#[allow(clippy::too_many_arguments)]
fn run_burn(
    app: &AppHandle,
    ffmpeg_bin: &Path,
    video: &Path,
    ass_name: &str,
    tmp_dir: &Path,
    part_path: &Path,
    duration_cs: Option<i64>,
    transcode_audio: bool,
) -> Result<(), String> {
    let mut cmd = quiet(Command::new(ffmpeg_bin));
    cmd.current_dir(tmp_dir)
        .arg("-i")
        .arg(video)
        .args(["-vf", &format!("ass={ass_name}")])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "21"]);
    if transcode_audio {
        cmd.args(["-c:a", "aac", "-b:a", "192k"]);
    } else {
        cmd.args(["-c:a", "copy"]);
    }
    cmd.args(["-progress", "pipe:1", "-nostats", "-hide_banner", "-loglevel", "error", "-y", "-f", "mp4"])
        .arg(part_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("שגיאה בהפעלת ffmpeg: {e}"))?;

    // Drain stderr on its own thread so a chatty encoder can't deadlock us.
    let stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut s) = stderr {
            use std::io::Read;
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });

    if let Some(stdout) = child.stdout.take() {
        let reader = std::io::BufReader::new(stdout);
        let mut last_pct: u8 = 0;
        for line in reader.lines().map_while(Result::ok) {
            // `-progress` emits `out_time_us=<microseconds>` per tick.
            if let Some(us) = line.strip_prefix("out_time_us=") {
                if let (Ok(us), Some(total_cs)) = (us.trim().parse::<i64>(), duration_cs) {
                    if total_cs > 0 {
                        let pct = ((us / 10_000).clamp(0, total_cs) * 100 / total_cs) as u8;
                        // Cap at 99 — 100 is reserved for the final rename.
                        let pct = pct.min(99);
                        if pct > last_pct {
                            last_pct = pct;
                            emit_progress(app, pct);
                        }
                    }
                }
            }
        }
    }

    let status = child.wait().map_err(|e| format!("שגיאה בהמתנה ל-ffmpeg: {e}"))?;
    let stderr_text = err_thread.join().unwrap_or_default();

    let ok = status.success()
        && part_path.metadata().map(|m| m.len() > 0).unwrap_or(false);
    if !ok {
        log::error!("burn ffmpeg failed (audio_transcode={transcode_audio}): {stderr_text}");
        let tail: String = stderr_text
            .chars()
            .rev()
            .take(300)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return Err(format!("צריבת הכתוביות נכשלה. פרטים: {}", tail.trim()));
    }
    Ok(())
}

/// Probes the video duration (centiseconds) by parsing the `Duration:` line of
/// `ffmpeg -i` stderr — the shipped asset has no ffprobe. `None` ⇒ progress
/// events are skipped but the burn proceeds.
fn probe_duration_cs(ffmpeg_bin: &Path, video: &Path) -> Option<i64> {
    let out = quiet(Command::new(ffmpeg_bin))
        .arg("-i")
        .arg(video)
        .arg("-hide_banner")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    parse_duration_cs(&String::from_utf8_lossy(&out.stderr))
}

/// Finds `Duration: HH:MM:SS.cc` in ffmpeg banner stderr.
fn parse_duration_cs(stderr: &str) -> Option<i64> {
    let rest = stderr.split("Duration: ").nth(1)?;
    let ts = rest.split([',', '\n', '\r']).next()?.trim();
    if ts.starts_with('N') {
        return None; // "N/A"
    }
    let mut parts = ts.split(':');
    let h: i64 = parts.next()?.trim().parse().ok()?;
    let m: i64 = parts.next()?.trim().parse().ok()?;
    let sec: f64 = parts.next()?.trim().parse().ok()?;
    Some((h * 3600 + m * 60) * 100 + (sec * 100.0).round() as i64)
}

/// `movie.mp4` → `movie.subtitled.mp4` next to the source; if taken,
/// `movie.subtitled.2.mp4`, `.3.`, … — never silently overwrite.
fn free_output_path(video: &Path) -> PathBuf {
    let stem = video.file_stem().and_then(|s| s.to_str()).unwrap_or("video");
    let first = video.with_file_name(format!("{stem}.subtitled.mp4"));
    if !first.exists() {
        return first;
    }
    for n in 2.. {
        let p = video.with_file_name(format!("{stem}.subtitled.{n}.mp4"));
        if !p.exists() {
            return p;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_parses_from_banner() {
        let s = "Input #0, mov,mp4 …\n  Duration: 00:00:46.51, start: 0.000000, bitrate: 433 kb/s\n";
        assert_eq!(parse_duration_cs(s), Some(4651));
        assert_eq!(parse_duration_cs("  Duration: 01:02:03.04, x"), Some(372304));
        assert_eq!(parse_duration_cs("Duration: N/A, start"), None);
        assert_eq!(parse_duration_cs("no duration here"), None);
    }

    #[test]
    fn output_path_appends_numeric_suffix() {
        let dir = std::env::temp_dir().join(format!("tl_burn_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("סרט שלי.mp4");
        let first = free_output_path(&video);
        assert_eq!(first.file_name().unwrap().to_str().unwrap(), "סרט שלי.subtitled.mp4");
        std::fs::write(&first, b"x").unwrap();
        let second = free_output_path(&video);
        assert_eq!(second.file_name().unwrap().to_str().unwrap(), "סרט שלי.subtitled.2.mp4");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
