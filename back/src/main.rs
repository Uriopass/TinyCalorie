mod db;
mod migrate;
mod search;

use crate::search::SearchItem;
use axum::extract::Path;
use axum::http::header::CONTENT_TYPE;
use axum::response::{AppendHeaders, Html};
use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Extension, Json, Router,
};
use chrono::Utc;
use db::Database;
use include_dir::{include_dir, Dir};
use r2d2_sqlite::rusqlite::{params, Connection};
use search::Searcher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;

pub static MIGRATIONS: Dir = include_dir!("migrations");

#[derive(Serialize, Deserialize)]
struct Item {
    id: u64,
    name: String,
    calories: f64,
    multiplier: f64,
    timestamp: u64,
}

#[derive(Serialize)]
struct Summary {
    total: f64,
    items: Vec<Item>,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            total: 0.0,
            items: vec![],
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AddItem {
    name: String,
    calories: f64,
    multiplier: f64,
}

#[derive(Serialize, Deserialize)]
struct RemoveItem {
    id: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let db = Database::new("db.db").expect("could not open db");

    tracing::info!(
        "sqlite version: {}",
        db.connection()
            .unwrap()
            .query_row("select sqlite_version();", [], |v| v
                .get::<usize, String>(0))
            .unwrap()
    );

    migrate::migrate(&db.0, &MIGRATIONS).expect("could not run migrations");
    let matcher = Searcher::new(&*db.connection().expect("could not get connection"));

    let app = Router::new()
        .route("/", get(root))
        .route("/icon.ico", get(icon))
        .route("/api/conf", get(get_conf).post(set_conf))
        .route("/api/item", post(add_item))
        .route("/api/item/:id", delete(remove_item))
        .route("/api/autocomplete/:qry", get(autocomplete))
        .route("/api/summary", get(summary))
        .route("/api/summary/:date", get(summary_date))
        .layer(Extension(matcher))
        .layer(db);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(3001);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn root() -> Html<&'static str> {
    tracing::info!("rendering root");
    Html(include_str!("../index.html"))
}

const ICON_FILE: &[u8] = include_bytes!("../icon.ico");

async fn icon() -> impl IntoResponse {
    tracing::info!("rendering icon");
    (
        AppendHeaders([(CONTENT_TYPE, "application/x-icon")]),
        ICON_FILE,
    )
}

async fn autocomplete(
    Path(qry): Path<String>,
    Extension(search): Extension<Searcher>,
) -> impl IntoResponse {
    tracing::info!("autocomplete: {}", &qry);
    let res = search.search(&qry);
    (StatusCode::OK, Json(res))
}

async fn summary_date(
    Path(date): Path<String>,
    Extension(db): Extension<Database>,
) -> impl IntoResponse {
    tracing::info!("getting historical summary");
    // YYYY-MM-DD validation
    if date.len() != 10 {
        return (StatusCode::BAD_REQUEST, Json(Summary::default()));
    }
    let v: Vec<&str> = date.split("-").collect();
    if v.len() != 3 || v[0].len() != 4 || v[1].len() != 2 || v[2].len() != 2 {
        return (StatusCode::BAD_REQUEST, Json(Summary::default()));
    }

    let conn = db.connection().expect("could not get connection");
    let summary = mk_summary(&*conn, date);
    (StatusCode::OK, Json(summary))
}

async fn summary(Extension(db): Extension<Database>) -> impl IntoResponse {
    tracing::info!("getting summary");
    let conn = db.connection().expect("could not get connection");
    let now = Utc::now().with_timezone(&chrono_tz::Europe::Paris);
    let date = now.date().format("YYYY-MM-DD").to_string();
    let summary = mk_summary(&*conn, date);
    (StatusCode::OK, Json(summary))
}

fn mk_summary(conn: &Connection, date: String) -> Summary {
    let mut qry = conn
        .prepare_cached(
            "SELECT id, name, calories, multiplier, timestamp FROM items WHERE date = ?1",
        )
        .expect("could not prepare qry");
    let mut rows = qry.query(&[&date]).expect("could not run qry");

    let mut items = vec![];
    while let Ok(Some(x)) = rows.next() {
        items.push(Item {
            id: x.get("id").unwrap(),
            name: x.get("name").unwrap(),
            calories: x.get("calories").unwrap(),
            multiplier: x.get("multiplier").unwrap(),
            timestamp: x.get("timestamp").unwrap(),
        });
    }

    items.sort_by_key(|x| x.timestamp);

    Summary {
        total: items.iter().map(|x| x.calories * x.multiplier).sum(),
        items,
    }
}

#[derive(Deserialize)]
pub struct ConfSet {
    key: String,
    value: String,
}

async fn set_conf(
    Extension(db): Extension<Database>,
    Json(confset): Json<ConfSet>,
) -> impl IntoResponse {
    tracing::info!("setting conf: {} = {}", &confset.key, &confset.value);
    let conn = db.connection().expect("could not get connection");
    conn.execute(
        "INSERT INTO conf (key, value) VALUES (?1, ?2) ON CONFLICT DO UPDATE SET value = ?2;",
        params![confset.key, confset.value],
    )
    .expect("could not prepare qry");
    StatusCode::CREATED
}

async fn get_conf(Extension(db): Extension<Database>) -> impl IntoResponse {
    tracing::info!("getting conf");
    let conn = db.connection().expect("could not get connection");
    let mut qry = conn
        .prepare("SELECT key, value FROM conf;")
        .expect("could not prepare qry");
    let mut rows = qry.query([]).expect("could not do query");
    let mut v: HashMap<String, String> = HashMap::new();

    while let Ok(Some(row)) = rows.next() {
        v.insert(row.get_unwrap("key"), row.get_unwrap("value"));
    }

    (StatusCode::CREATED, Json(v))
}

async fn add_item(
    Extension(db): Extension<Database>,
    Extension(search): Extension<Searcher>,
    Json(item): Json<AddItem>,
) -> impl IntoResponse {
    tracing::info!("adding item {:?}", item);
    let conn = db.connection().expect("could not get connection");
    let now = Utc::now().with_timezone(&chrono_tz::Europe::Paris);
    let id = conn
        .query_row(
            "INSERT INTO items (name, calories, multiplier, date, timestamp) VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id;",
            params![
            item.name,
            item.calories,
            item.multiplier,
            now.date().format("YYYY-MM-DD").to_string(),
            now.timestamp()
        ]
                , |row| {
                row.get("id")
            }
        )
        .expect("could not prepare qry");
    search.insert(
        id,
        SearchItem {
            name: item.name,
            calories: item.calories,
        },
    );
    StatusCode::CREATED
}

async fn remove_item(
    Extension(db): Extension<Database>,
    Extension(search): Extension<Searcher>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    tracing::info!("removing item {}", id);
    let conn = db.connection().expect("could not get connection");
    if let Err(e) = conn.execute("DELETE FROM items WHERE id = ?1;", &[&id]) {
        tracing::error!("error in remove_item: {}", e);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    search.remove(id);
    StatusCode::CREATED
}
