use crate::llm::{err, structured_completion, DailyLimit};
use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MODEL: &str = "anthropic/claude-opus-5.5";
static LIMIT: DailyLimit = DailyLimit::new("PHOTO_DAILY_LIMIT", 50);

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

pub async fn estimate_photo(Json(req): Json<PhotoRequest>) -> impl IntoResponse {
    const LABEL: &str = "photo estimate";
    if !matches!(
        req.media_type.as_str(),
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    ) {
        return err(LABEL, StatusCode::BAD_REQUEST, "unsupported image type");
    }
    if !LIMIT.take() {
        return err(
            LABEL,
            StatusCode::TOO_MANY_REQUESTS,
            "daily photo limit reached, try again tomorrow",
        );
    }

    let content = json!([
        {"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", req.media_type, req.image)}},
        {"type": "text", "text": PROMPT}
    ]);
    let schema = json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "calories": {"type": "number"},
            "multiplier": {"type": "number"}
        },
        "required": ["name", "calories", "multiplier"],
        "additionalProperties": false
    });
    let text =
        match structured_completion(LABEL, MODEL, "medium", 16000, None, content, schema).await {
            Ok(t) => t,
            Err(e) => return e,
        };
    match serde_json::from_str::<PhotoEstimate>(&text) {
        Ok(estimate) => (StatusCode::OK, Json(json!(estimate))),
        Err(_) => err(
            LABEL,
            StatusCode::BAD_GATEWAY,
            "could not parse model output",
        ),
    }
}
