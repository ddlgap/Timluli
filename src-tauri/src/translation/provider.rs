//! HTTP clients for the OpenAI-compatible Groq and Cerebras chat endpoints.
//! One request translates one batch: the user message is a JSON object
//! `{id: text}`, and the model must reply with the same keys translated.

use std::collections::HashMap;

pub const GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const CEREBRAS_URL: &str = "https://api.cerebras.ai/v1/chat/completions";
pub const GROQ_MODELS_URL: &str = "https://api.groq.com/openai/v1/models";
pub const CEREBRAS_MODELS_URL: &str = "https://api.cerebras.ai/v1/models";

pub fn base_url(provider: &str) -> &'static str {
    match provider {
        "groq" => GROQ_URL,
        "cerebras" => CEREBRAS_URL,
        _ => "",
    }
}

fn models_url(provider: &str) -> &'static str {
    match provider {
        "groq" => GROQ_MODELS_URL,
        "cerebras" => CEREBRAS_MODELS_URL,
        _ => "",
    }
}

/// A chat/text model offered by a provider, as surfaced to the settings UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub context_window: Option<u32>,
    pub max_completion_tokens: Option<u32>,
}

/// Live rate-limit budget read from a provider's response headers. Used by the
/// paid (parallel) path to apply backpressure before hitting a hard 429. We track
/// the per-minute *token* budget (the binding constraint for both providers) and
/// its reset window; request counts are not used for short cooldowns.
#[derive(Debug, Clone, Default)]
pub struct RateInfo {
    pub remaining_tokens: Option<u64>,
    pub reset_secs: Option<u64>,
}

/// Parses the leading numeric portion of a header value, tolerating unit
/// suffixes like `"7.66s"` (→ 8) that providers attach to reset headers.
fn parse_leading_number(v: &str) -> Option<u64> {
    let s: String = v
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    s.parse::<f64>().ok().map(|f| f.ceil() as u64)
}

fn header_num(headers: &reqwest::header::HeaderMap, names: &[&str]) -> Option<u64> {
    for n in names {
        if let Some(val) = headers.get(*n).and_then(|v| v.to_str().ok()) {
            if let Some(num) = parse_leading_number(val) {
                return Some(num);
            }
        }
    }
    None
}

/// Reads rate-limit budget from response headers. Groq and Cerebras use
/// different header names, so we try both families.
fn parse_rate_info(headers: &reqwest::header::HeaderMap) -> RateInfo {
    RateInfo {
        remaining_tokens: header_num(
            headers,
            &[
                "x-ratelimit-remaining-tokens",
                "x-ratelimit-remaining-tokens-minute",
            ],
        ),
        reset_secs: header_num(
            headers,
            &[
                "x-ratelimit-reset-tokens",
                "x-ratelimit-reset-tokens-minute",
            ],
        ),
    }
}

/// Fetches the provider's chat/text model catalogue via its OpenAI-compatible
/// `/models` endpoint, filtering out non-text models (STT/TTS/guard/vision).
pub async fn fetch_models(
    client: &reqwest::Client,
    provider: &str,
    api_key: &str,
) -> Result<Vec<ModelInfo>, String> {
    let url = models_url(provider);
    if url.is_empty() {
        return Err(format!("ספק לא נתמך: {provider}"));
    }

    let resp = client
        .get(url)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|e| format!("שגיאת רשת: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status.as_u16(), truncate(&text, 200)));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("תשובה לא תקינה מהשרת: {e}"))?;

    let data = json["data"].as_array().ok_or("רשימת מודלים ריקה")?;
    let mut models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            // Groq exposes `active`; skip retired models. Cerebras omits it (treat as active).
            if m["active"].as_bool() == Some(false) {
                return None;
            }
            if !is_text_model(&id) {
                return None;
            }
            Some(ModelInfo {
                id,
                context_window: m["context_window"].as_u64().map(|v| v as u32),
                max_completion_tokens: m["max_completion_tokens"].as_u64().map(|v| v as u32),
            })
        })
        .collect();

    // Quality-first ordering so the best model is the top dropdown choice (the
    // "automatic" option still leads). Ties break alphabetically.
    models.sort_by(|a, b| {
        model_rank(&a.id)
            .cmp(&model_rank(&b.id))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(models)
}

/// Hebrew-translation quality rank for the settings dropdown (lower = better, shown
/// first). Derived from a live benchmark of both providers' models. Unknown models
/// sort in the middle (after proven-good, before known-poor for Hebrew).
fn model_rank(id: &str) -> u8 {
    let l = id.to_lowercase();
    if l.contains("gpt-oss-120b") {
        0
    } else if l.contains("llama-4-scout") {
        1
    } else if l.contains("llama-3.3-70b") {
        2
    } else if l.contains("qwen-3-235b") {
        3
    } else if l.contains("gpt-oss-20b") {
        4
    } else if l.contains("llama-3.1-8b") || l.contains("llama3.1-8b") {
        5
    } else if l.contains("zai-glm") || l.contains("glm-4") {
        6
    } else if l.contains("qwen3-32b") || l.contains("qwen-3-32b") {
        // reasoning leaks into the translation output — avoid for Hebrew
        90
    } else if l.contains("compound") {
        // agentic system: slow, 413s on normal inputs
        91
    } else if l.contains("allam") {
        // Arabic-focused: produces garbled/looping Hebrew
        92
    } else {
        40
    }
}

/// Heuristic: keep only text-generation chat models. The `/models` payload has no
/// modality field, so we exclude by well-known id substrings (audio, safety,
/// vision, embedding models).
fn is_text_model(id: &str) -> bool {
    let lower = id.to_lowercase();
    const EXCLUDE: &[&str] = &[
        "whisper", "tts", "guard", "safeguard", "embed", "-vl-", "vision", "playai",
    ];
    !EXCLUDE.iter().any(|needle| lower.contains(needle))
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
    let m = body.to_lowercase();
    // A *daily* cap (e.g. Cerebras free's 1M tokens/day) is often reported as a
    // 429, but it won't reset within this job — so treat it as `Quota` (permanent
    // skip → fall straight through to the next provider) instead of retrying the
    // same model. Per-*minute* limits below keep retrying the same model.
    let daily_exhausted = m.contains("tokens per day")
        || m.contains("requests per day")
        || m.contains("tokens_per_day")
        || m.contains("daily")
        || m.contains("quota_exceeded")
        || m.contains("token_quota");
    match code {
        429 if daily_exhausted => TranslateError::Quota(msg),
        429 => TranslateError::RateLimit(msg, retry_after),
        402 => TranslateError::Quota(msg),
        413 => TranslateError::Transient(msg),
        _ => {
            if m.contains("payment") || m.contains("insufficient_quota") || daily_exhausted {
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

/// Translates one batch of `(id, text)` pairs, returning a map of id → translated
/// text plus the live rate-limit budget read from the response headers.
#[allow(clippy::too_many_arguments)]
pub async fn translate_batch(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    api_key: &str,
    target_language: &str,
    system_prompt: &str,
    batch: &[(usize, &str)],
    max_output_tokens: u32,
) -> Result<(HashMap<String, String>, RateInfo), TranslateError> {
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
        // Providers count this reservation against the per-minute token limit, and
        // on free Cerebras it must also fit the ~8K context window. The caller sizes
        // it per provider+tier (see `profile_for`): small on free, larger on paid.
        "max_completion_tokens": max_output_tokens,
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

    let rate = parse_rate_info(resp.headers());

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TranslateError::Transient(e.to_string()))?;
    let raw = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    let cleaned = strip_fences(raw);
    let map = serde_json::from_str::<HashMap<String, String>>(&cleaned).map_err(|_| {
        TranslateError::Transient(format!("Model returned invalid JSON: {}", truncate(raw, 200)))
    })?;
    Ok((map, rate))
}
