//! Groq cloud speech-to-text backend.
//!
//! Uploads the audio file to Groq's OpenAI-compatible transcription endpoint
//! using `whisper-large-v3-turbo`. Reuses the shared Groq API key stored
//! (DPAPI-encrypted) in `secrets.json`.

use std::path::Path;
use tauri::{AppHandle, Emitter};

const GROQ_STT_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const GROQ_STT_MODEL: &str = "whisper-large-v3-turbo";

pub async fn transcribe(app: &AppHandle, input: &Path) -> Result<String, String> {
    let key = crate::secrets::get_key(app, "groq").ok_or_else(|| {
        "לא הוגדר מפתח Groq. חבר שירות תרגום בהגדרות → תרגום מסמכים כדי לתמלל בענן.".to_string()
    })?;

    // Single cloud request — surface an indeterminate progress tick so the mic
    // bubble matches the local backend's behavior.
    let _ = app.emit_to(
        "mic",
        "speakly://transcribe-progress",
        serde_json::json!({ "chunk": 1, "total": 1 }),
    );

    let bytes = tokio::fs::read(input)
        .await
        .map_err(|e| format!("שגיאה בקריאת הקובץ: {e}"))?;
    let filename = input
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("audio")
        .to_string();

    let file_part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str("application/octet-stream")
        .map_err(|e| format!("שגיאה בהכנת הקובץ: {e}"))?;

    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", GROQ_STT_MODEL)
        .text("language", "he")
        .text("response_format", "text");

    let client = reqwest::Client::new();
    let resp = client
        .post(GROQ_STT_URL)
        .bearer_auth(&key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("שגיאת רשת בתמלול בענן: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(match status.as_u16() {
            401 | 403 => "מפתח Groq לא תקין. בדוק את החיבור לשירות בהגדרות.".into(),
            413 => "קובץ האודיו גדול מדי לתמלול בענן. נסה מנוע מקומי או קובץ קצר יותר.".into(),
            429 => "חרגת ממכסת השימוש ב-Groq. נסה שוב מאוחר יותר או השתמש במנוע מקומי.".into(),
            _ => format!("התמלול בענן נכשל ({status}): {}", body.trim()),
        });
    }

    // `response_format=text` returns the raw transcript as the body.
    let text = resp
        .text()
        .await
        .map_err(|e| format!("שגיאה בקריאת התשובה: {e}"))?;
    Ok(text)
}
