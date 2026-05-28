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
    /// Bad JSON, network blip, other HTTP error — retry the batch on the next model.
    Transient(String),
}

fn is_quota(code: u16, msg: &str) -> bool {
    if code == 402 || code == 429 {
        return true;
    }
    let m = msg.to_lowercase();
    [
        "rate_limit",
        "rate limit",
        "payment_required",
        "payment required",
        "quota",
        "tokens per minute",
        "insufficient_quota",
    ]
    .iter()
    .any(|p| m.contains(p))
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
        "max_completion_tokens": 8192,
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
        let text = resp.text().await.unwrap_or_default();
        let msg = format!("HTTP {}: {}", status.as_u16(), truncate(&text, 200));
        if is_quota(status.as_u16(), &text) {
            return Err(TranslateError::Quota(msg));
        }
        return Err(TranslateError::Transient(msg));
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
