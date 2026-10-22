use crate::llm::{err, structured_completion, DailyLimit};
use crate::search::Searcher;
use axum::{http::StatusCode, response::IntoResponse, Extension, Json};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;

// gpt-oss-120b on Cerebras answers in ~0.4s with about the same accuracy on the user's
// conventions as Claude Sonnet 5.5 (~1.6s) and Haiku 5.5 (~1.5s). Groq was as fast but returned
// broken JSON, so Cerebras goes first and the other hosts are only a fallback.
const MODEL: &str = "openai/gpt-oss-120b";
static LIMIT: DailyLimit = DailyLimit::new("GUESS_DAILY_LIMIT", 500);
static CACHE: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);

const PROMPT: &str = "You estimate calories for a calorie counter. The user typed a food name, often in French.
Return the kcal for exactly ONE of what the name describes:
- If the name starts with a unit and no number, like \"g de riz\", \"100g de poulet\", \"cl de vin\", \"cuillère d'huile\", return kcal for one such unit (1 g of rice, 100 g of chicken, 1 cl of wine...). The user puts the amount in a separate multiplier field.
- If it has a quantity, like \"200g de riz\" or \"2 oeufs\", return kcal for that whole quantity.
- Otherwise return kcal for one usual portion (one apple, one Big Mac, one plate of pasta).
Weights are as bought, before cooking: rice, pasta, semolina, couscous, oats, lentils and other dry foods are weighed DRY (\"g de riz\" is about 3.5 kcal, \"g de pâtes\" about 3.6), meat and fish raw. Only when dry makes no sense (yogurt, fromage blanc, soup, milk, cheese, bread, fruit) use the food as it is.
Sport and other activities burn calories: return them negative.
When the user's past entries below cover the same food or activity, scale from their numbers rather than general knowledge.";

const HISTORY_INTRO: &str = "\nThe user's own past entries for similar foods (use them to match their portions and habits):\n";

#[derive(Deserialize)]
pub struct GuessRequest {
    name: String,
}

#[derive(Deserialize)]
struct Guess {
    calories: f64,
}

pub async fn guess_calories(
    Json(req): Json<GuessRequest>,
    Extension(search): Extension<Searcher>,
) -> impl IntoResponse {
    const LABEL: &str = "name guess";
    let name = req.name.trim().to_lowercase();
    if name.chars().count() < 2 || name.chars().count() > 100 {
        return err(
            LABEL,
            StatusCode::BAD_REQUEST,
            "name must be 2 to 100 characters",
        );
    }
    // Already logged under this name: the user's own number beats any guess.
    if let Some(calories) = search.exact(&name) {
        return (StatusCode::OK, Json(json!({ "calories": calories })));
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
    let mut prompt = PROMPT.to_string();
    let similar = search.similar(&name, 8);
    if !similar.is_empty() {
        prompt.push_str(HISTORY_INTRO);
        for (n, c) in similar {
            prompt.push_str(&format!("- \"{}\": {} kcal\n", n, c));
        }
    }
    prompt.push_str(&format!("\nFood: {}", name));
    let content = json!(prompt);
    let text = match structured_completion(
        LABEL,
        MODEL,
        "low",
        2000,
        Some(json!({"order": ["Cerebras"]})),
        content,
        schema,
    )
    .await
    {
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
        Err(_) => err(
            LABEL,
            StatusCode::BAD_GATEWAY,
            "could not parse model output",
        ),
    }
}
