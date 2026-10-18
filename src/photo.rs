use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MODEL: &str = "anthropic/claude-opus-5.5";

const PROMPT: &str = "This is a photo of food someone is about to eat. Estimate its calories for a calorie counter.
- name: short description of the food, a few words, e.g. \"Pepperoni pizza slice\". If there are several foods, name them all briefly, e.g. \"Steak, fries, salad\".
- calories: kcal of ONE unit (one slice, one egg, one plate...).
- multiplier: how many units are in the photo (usually 1).
Use visible packaging or nutrition labels if present. Otherwise estimate portion sizes from the image. Give your best single guess, not a range.";

#[derive(Deserialize)]
pub struct PhotoRequest {
    /// base64-encoded image, without the data: prefix
    image: String,
    media_type: String,
}

#[derive(Serialize, Deserialize)]
pub struct PhotoEstimate {
    name: String,
    calories: f64,
    multiplier: f64,
}

#[derive(Serialize)]
struct PhotoError {
    error: String,
}

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    let error = msg.into();
    tracing::error!("photo estimate: {}", error);
    (status, Json(json!(PhotoError { error })))
}

pub async fn estimate_photo(Json(req): Json<PhotoRequest>) -> impl IntoResponse {
    let api_key = match std::env::var("OPENROUTER_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "OPENROUTER_API_KEY is not set on the server",
            )
        }
    };
    if !matches!(
        req.media_type.as_str(),
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    ) {
        return err(StatusCode::BAD_REQUEST, "unsupported image type");
    }

    let body = json!({
        "model": MODEL,
        "max_tokens": 16000,
        "reasoning": {"effort": "medium"},
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "calorie_estimate",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "calories": {"type": "number"},
                        "multiplier": {"type": "number"}
                    },
                    "required": ["name", "calories", "multiplier"],
                    "additionalProperties": false
                }
            }
        },
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", req.media_type, req.image)}},
                {"type": "text", "text": PROMPT}
            ]
        }]
    });

    let resp = reqwest::Client::new()
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(api_key)
        .header("X-Title", "TinyCalorie")
        .json(&body)
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("request failed: {}", e)),
    };
    let status = resp.status();
    let msg: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("bad response: {}", e)),
    };
    if !status.is_success() {
        return err(
            StatusCode::BAD_GATEWAY,
            format!("API error {}: {}", status, msg["error"]["message"]),
        );
    }

    let usage = &msg["usage"];
    tracing::info!(
        "photo estimate: model={} prompt_tokens={} completion_tokens={} (reasoning={}) cost=${}",
        msg["model"],
        usage["prompt_tokens"],
        usage["completion_tokens"],
        usage["completion_tokens_details"]["reasoning_tokens"],
        usage["cost"]
    );

    let choice = &msg["choices"][0];
    match choice["finish_reason"].as_str() {
        Some("stop") => {}
        other => {
            return err(
                StatusCode::BAD_GATEWAY,
                format!("model stopped early: {}", other.unwrap_or("unknown")),
            )
        }
    }

    let text = choice["message"]["content"].as_str();
    let estimate: PhotoEstimate = match text.map(serde_json::from_str) {
        Some(Ok(e)) => e,
        _ => return err(StatusCode::BAD_GATEWAY, "could not parse model output"),
    };
    (StatusCode::OK, Json(json!(estimate)))
}
