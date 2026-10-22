use axum::{http::StatusCode, Json};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

pub type ApiError = (StatusCode, Json<Value>);

pub fn err(label: &str, status: StatusCode, msg: impl Into<String>) -> ApiError {
    let error = msg.into();
    tracing::error!("{}: {}", label, error);
    (status, Json(json!({ "error": error })))
}

/// The app has no auth, so paid API calls are capped per day.
pub struct DailyLimit {
    env_var: &'static str,
    default: u32,
    calls: Mutex<(String, u32)>,
}

impl DailyLimit {
    pub const fn new(env_var: &'static str, default: u32) -> Self {
        Self {
            env_var,
            default,
            calls: Mutex::new((String::new(), 0)),
        }
    }

    /// Returns false if today's budget is used up, otherwise counts one call.
    pub fn take(&self) -> bool {
        let limit = std::env::var(self.env_var)
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(self.default);
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut calls = self.calls.lock().unwrap();
        if calls.0 != today {
            *calls = (today, 0);
        }
        if calls.1 >= limit {
            return false;
        }
        calls.1 += 1;
        true
    }
}

// Reused across requests so the TLS connection to OpenRouter stays warm.
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Sends one user message to OpenRouter and returns the model's JSON text, which follows `schema`.
/// `provider` is OpenRouter's provider routing object, e.g. {"order": ["Cerebras"]}.
pub async fn structured_completion(
    label: &str,
    model: &str,
    effort: &str,
    max_tokens: u32,
    provider: Option<Value>,
    content: Value,
    schema: Value,
) -> Result<String, ApiError> {
    let api_key = match std::env::var("OPENROUTER_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            return Err(err(
                label,
                StatusCode::SERVICE_UNAVAILABLE,
                "OPENROUTER_API_KEY is not set on the server",
            ))
        }
    };

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "reasoning": {"effort": effort},
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "calorie_estimate", "strict": true, "schema": schema}
        },
        "messages": [{"role": "user", "content": content}]
    });
    if let Some(provider) = provider {
        body["provider"] = provider;
    }

    let resp = CLIENT
        .get_or_init(reqwest::Client::new)
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(api_key)
        .header("X-Title", "TinyCalorie")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            err(
                label,
                StatusCode::BAD_GATEWAY,
                format!("request failed: {}", e),
            )
        })?;
    let status = resp.status();
    let msg: Value = resp.json().await.map_err(|e| {
        err(
            label,
            StatusCode::BAD_GATEWAY,
            format!("bad response: {}", e),
        )
    })?;
    if !status.is_success() {
        return Err(err(
            label,
            StatusCode::BAD_GATEWAY,
            format!("API error {}: {}", status, msg["error"]["message"]),
        ));
    }

    let usage = &msg["usage"];
    tracing::info!(
        "{}: model={} provider={} prompt_tokens={} completion_tokens={} (reasoning={}) cost=${}",
        label,
        msg["model"],
        msg["provider"],
        usage["prompt_tokens"],
        usage["completion_tokens"],
        usage["completion_tokens_details"]["reasoning_tokens"],
        usage["cost"]
    );

    let choice = &msg["choices"][0];
    if choice["finish_reason"].as_str() != Some("stop") {
        return Err(err(
            label,
            StatusCode::BAD_GATEWAY,
            format!("model stopped early: {}", choice["finish_reason"]),
        ));
    }
    choice["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| err(label, StatusCode::BAD_GATEWAY, "empty model output"))
}
