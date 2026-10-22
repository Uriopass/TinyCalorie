use crate::llm::{err, structured_completion, DailyLimit};
use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;

// Sonnet at low effort answered in ~1.8s and, unlike Haiku, kept "g de pâtes" at cooked weight.
const MODEL: &str = "anthropic/claude-sonnet-5.5";
static LIMIT: DailyLimit = DailyLimit::new("GUESS_DAILY_LIMIT", 500);
static CACHE: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);

const PROMPT: &str = "You estimate calories for a calorie counter. The user typed a food name, often in French.
Return the kcal for exactly ONE of what the name describes:
- If the name starts with a unit and no number, like \"g de riz\", \"100g de poulet\", \"cl de vin\", \"cuillère d'huile\", return kcal for one such unit (1 g of rice, 100 g of chicken, 1 cl of wine...). The user puts the amount in a separate multiplier field.
- If it has a quantity, like \"200g de riz\" or \"2 oeufs\", return kcal for that whole quantity.
- Otherwise return kcal for one usual portion (one apple, one Big Mac, one plate of pasta).
Assume food as usually eaten (cooked rice and pasta, not dry) unless the name says otherwise (\"cru\", \"sec\", \"raw\").
Food: ";

#[derive(Deserialize)]
pub struct GuessRequest {
    name: String,
}

#[derive(Deserialize)]
struct Guess {
    calories: f64,
}

pub async fn guess_calories(Json(req): Json<GuessRequest>) -> impl IntoResponse {
    const LABEL: &str = "name guess";
    let name = req.name.trim().to_lowercase();
    if name.chars().count() < 2 || name.chars().count() > 100 {
        return err(LABEL, StatusCode::BAD_REQUEST, "name must be 2 to 100 characters");
    }
    if let Some(&calories) = CACHE.lock().unwrap().as_ref().and_then(|c| c.get(&name)) {
        return (StatusCode::OK, Json(json!({ "calories": calories })));
    }
    if !LIMIT.take() {
        return err(
            LABEL,
            StatusCode::TOO_MANY_REQUESTS,
            "daily guess limit reached, try again tomorrow",
        );
    }

    let schema = json!({
        "type": "object",
        "properties": {"calories": {"type": "number"}},
        "required": ["calories"],
        "additionalProperties": false
    });
    let content = json!(format!("{}{}", PROMPT, name));
    let text = match structured_completion(LABEL, MODEL, "low", 2000, content, schema).await {
        Ok(t) => t,
        Err(e) => return e,
    };
    match serde_json::from_str::<Guess>(&text) {
        Ok(g) => {
            CACHE
                .lock()
                .unwrap()
                .get_or_insert_with(HashMap::new)
                .insert(name, g.calories);
            (StatusCode::OK, Json(json!({ "calories": g.calories })))
        }
        Err(_) => err(LABEL, StatusCode::BAD_GATEWAY, "could not parse model output"),
    }
}
