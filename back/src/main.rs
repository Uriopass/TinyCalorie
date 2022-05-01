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
    date: String,
    conf: HashMap<String, String>,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            total: 0.0,
            items: vec![],
            date: "".to_string(),
            conf: Default::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct EditItem {
    name: Option<String>,
    calories: Option<f64>,
    multiplier: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct AddItem {
    name: String,
    calories: f64,
    multiplier: f64,
    date: String,
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
        .route("/api/item/:id", delete(remove_item).put(edit_item))
        .route("/api/autocomplete/:qry", get(autocomplete))
        .route("/api/summary/:date", get(summary))
        .route("/api/calendar_data/:date", get(calendar_data))
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

#[cfg(debug_assertions)]
async fn root() -> impl IntoResponse {
    tracing::info!("rendering root");
    Html(std::fs::read_to_string("index.html").expect("could not find index.html"))
}

#[cfg(not(debug_assertions))]
async fn root() -> impl IntoResponse {
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

fn check_date(date: &str) -> bool {
    if date.len() != 10 {
        return false;
    }
    let v: Vec<&str> = date.split("-").collect();
    if v.len() != 3 || v[0].len() != 4 || v[1].len() != 2 || v[2].len() != 2 {
        return false;
    }
    true
}

async fn summary(
    Path(date): Path<String>,
    Extension(db): Extension<Database>,
) -> impl IntoResponse {
    tracing::info!("getting historical summary");
    // YYYY-MM-DD validation
    if !check_date(&date) {
        return (StatusCode::BAD_REQUEST, Json(Summary::default()));
    }
    let conn = db.connection().expect("could not get connection");
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
        date,
        conf: get_conf_from_db(&conn),
    }
}

#[derive(Serialize)]
pub struct CalendarItem {
    total: f64,
}

#[derive(Serialize, Default)]
pub struct CalendarData(HashMap<String, CalendarItem>);

async fn calendar_data(
    Path(date): Path<String>,
    Extension(db): Extension<Database>,
) -> impl IntoResponse {
    tracing::info!("getting calendar_data: {}", date);
    if date.len() != 7 || date.chars().nth(4).unwrap() != '-' {
        return (StatusCode::BAD_REQUEST, Json(Default::default()));
    }
    let (year, month) = date.split_once('-').expect("invalid format");
    let year: i64 = year.parse().expect("year is not integer");
    let month: i64 = month.parse().expect("month is not integer");

    let conn = db.connection().expect("could not get connection");

    let mut qry = conn.prepare_cached("SELECT date, sum(calories * multiplier) as total FROM items WHERE date BETWEEN ?1 AND ?2 GROUP BY date").expect("could not prepare qry");
    let mut rows = qry
        .query(params![date, format!("{}-{:0>2}", year, month + 1)])
        .expect("could not execute qry");

    let mut data = HashMap::with_capacity(32);
    while let Ok(Some(row)) = rows.next() {
        data.insert(
            row.get_unwrap("date"),
            CalendarItem {
                total: row.get_unwrap("total"),
            },
        );
    }

    (StatusCode::OK, Json(CalendarData(data)))
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

fn get_conf_from_db(conn: &Connection) -> HashMap<String, String> {
    let mut qry = conn
        .prepare("SELECT key, value FROM conf;")
        .expect("could not prepare qry");
    let mut rows = qry.query([]).expect("could not do query");
    let mut v: HashMap<String, String> = HashMap::new();

    while let Ok(Some(row)) = rows.next() {
        v.insert(row.get_unwrap("key"), row.get_unwrap("value"));
    }
    v
}

async fn get_conf(Extension(db): Extension<Database>) -> impl IntoResponse {
    tracing::info!("getting conf");
    let conn = db.connection().expect("could not get connection");

    (StatusCode::CREATED, Json(get_conf_from_db(&conn)))
}

async fn edit_item(
    Path(id): Path<u64>,
    Json(item): Json<EditItem>,
    Extension(db): Extension<Database>,
    Extension(search): Extension<Searcher>,
) -> impl IntoResponse {
    tracing::info!("editing item {:?}", item);
    let conn = db.connection().expect("could not get connection");
    let n_updated = conn
        .execute(
            "UPDATE items SET name = COALESCE(?1, name), calories = COALESCE(?2, calories), multiplier = COALESCE(?3, multiplier) WHERE id = ?4;",
            params![
            item.name,
            item.calories,
            item.multiplier,
            id,
        ])
        .expect("could not execute update item qry");
    if n_updated == 0 {
        return StatusCode::NOT_FOUND;
    }
    search.update(id, item.name, item.calories);
    StatusCode::OK
}

async fn add_item(
    Json(item): Json<AddItem>,
    Extension(db): Extension<Database>,
    Extension(search): Extension<Searcher>,
) -> impl IntoResponse {
    tracing::info!("adding item {:?}", item);
    if !check_date(&item.date) {
        return StatusCode::BAD_REQUEST;
    }
    let conn = db.connection().expect("could not get connection");
    let id = conn
        .query_row(
            "INSERT INTO items (name, calories, multiplier, date, timestamp) VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id;",
            params![
            item.name,
            item.calories,
            item.multiplier,
            item.date,
            Utc::now().timestamp()
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
