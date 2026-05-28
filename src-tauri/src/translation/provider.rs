//! HTTP clients for the OpenAI-compatible Groq and Cerebras chat endpoints.
//! One request translates one batch: the user message is a JSON object
//! `{id: text}`, and the model must reply with the same keys translated.

use std::collections::HashMap;

pub const GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const CEREBRAS_URL: &str = "https://api.cerebras.ai/v1/chat/completions";

pub fn base_url(provider: &str) -> &'static str {
    match provider {
        "groq" => GROQ_URL,
        "cerebras" => CEREBRAS_URL,
        _ => "",
    }
}

pub enum TranslateError {
    /// 402/429/quota/rate-limit — the model is exhausted; skip it permanently.
    Quota(String),
    /// 429 / per-minute rate limit — transient; wait and retry the SAME model.
    /// Carries an optional Retry-After (seconds).
    RateLimit(String, Option<u64>),
    /// Bad JSON, request-too-large (413), network blip — retry on the next model.
    Transient(String),
}

/// Classifies an unsuccessful HTTP response. Status code takes priority over
/// message sniffing so that e.g. a 413 "request too large (TPM)" cascades to the
/// next model instead of being mistaken for a permanent quota error.
fn classify(code: u16, body: &str, retry_after: Option<u64>, msg: String) -> TranslateError {
    match code {
        429 => TranslateError::RateLimit(msg, retry_after),
        402 => TranslateError::Quota(msg),
        413 => TranslateError::Transient(msg),
        _ => {
            let m = body.to_lowercase();
            if m.contains("payment") || m.contains("insufficient_quota") {
                TranslateError::Quota(msg)
            } else if m.contains("rate limit")
                || m.contains("rate_limit")
                || m.contains("tokens per minute")
            {
                TranslateError::RateLimit(msg, retry_after)
            } else {
                TranslateError::Transient(msg)
            }
        }
    }
}

fn strip_fences(text: &str) -> String {
    let t = text.trim();
    if !t.starts_with("```") {
        return t.to_string();
    }
    let mut s = &t[3..];
    if let Some(r) = s.strip_prefix("json") {
        s = r;
    }
    let s = s.trim_start().trim_end();
    let s = s.strip_suffix("```").unwrap_or(s);
    s.trim().to_string()
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Translates one batch of `(id, text)` pairs, returning a map of id → translated text.
pub async fn translate_batch(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    api_key: &str,
    target_language: &str,
    system_prompt: &str,
    batch: &[(usize, &str)],
) -> Result<HashMap<String, String>, TranslateError> {
    let mut payload = serde_json::Map::new();
    for (id, text) in batch {
        payload.insert(id.to_string(), serde_json::Value::String((*text).to_string()));
    }
    let user_content = serde_json::to_string(&payload)
        .map_err(|e| TranslateError::Transient(e.to_string()))?;

    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system_prompt.replace("{target_language}", target_language) },
            { "role": "user", "content": user_content },
        ],
        "temperature": 0.2,
        "top_p": 1,
        // Providers count this reservation against the per-minute token limit, so
        // keep it modest: a single request must fit well under free-tier TPM caps
        // (the smallest fallback models cap around 6000 TPM).
        "max_completion_tokens": 4096,
    });

    let resp = client
        .post(base_url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| TranslateError::Transient(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|f| f.ceil() as u64);
        let text = resp.text().await.unwrap_or_default();
        let msg = format!("HTTP {}: {}", status.as_u16(), truncate(&text, 200));
        return Err(classify(status.as_u16(), &text, retry_after, msg));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TranslateError::Transient(e.to_string()))?;
    let raw = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    let cleaned = strip_fences(raw);
    serde_json::from_str::<HashMap<String, String>>(&cleaned).map_err(|_| {
        TranslateError::Transient(format!("Model returned invalid JSON: {}", truncate(raw, 200)))
    })
}
