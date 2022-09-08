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

#[derive(Serialize, Default)]
struct WeightHistory {
    /// Contains the weights of the last X days
    weights: Vec<(String, f64)>,
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
struct AddWeight {
    date: String,
    weight: f64,
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

    let mut path = "db.db";
    if std::fs::metadata(path)
        .map(|x| !x.is_file())
        .unwrap_or(true)
        && std::fs::metadata("storage")
            .map(|x| x.is_dir())
            .unwrap_or(false)
    {
        tracing::info!("no db file found but a storage directory, going to put the db there.");
        path = "storage/db.db";
    }
    let db = Database::new(path).expect("could not open db");

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
        .route("/api/weight", post(add_weight))
        .route("/api/weight_history/:after_date", get(weight_history))
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
        .unwrap_or(80);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
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

async fn weight_history(
    Path(after_date): Path<String>,
    Extension(db): Extension<Database>,
) -> impl IntoResponse {
    tracing::info!("getting weight history after {}", after_date);
    let after_date = parse_date(&after_date);
    if after_date.is_none() {
        return (StatusCode::BAD_REQUEST, Json(WeightHistory::default()));
    }
    let after_date = after_date.unwrap();
    let conn = db.connection().expect("could not get connection");
    let mut stmt = conn
        .prepare("select date, weight from weight where date >= ? order by date")
        .expect("could not prepare statement");
    let mut rows = stmt
        .query_map(&[&after_date.to_year_month_day()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("could not query");
    let mut weights = vec![];
    while let Some(row) = rows.next() {
        let (date, weight): (String, f64) = row.expect("could not get row");
        weights.push((date, weight));
    }
    (StatusCode::OK, Json(WeightHistory { weights }))
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

struct Date {
    year: u32,
    month: u32,
    day: u32,
}

/// date is encoded as YYYY-MM-DD or YYYY-MM
fn parse_date(date: &str) -> Option<Date> {
    let v: Vec<&str> = date.split("-").collect();
    let year = v[0].parse().ok()?;
    if year < 1000 || year > 9999 {
        return None;
    }
    let month = v[1].parse().ok()?;
    if month < 1 || month > 12 {
        return None;
    }
    let day = if v.len() == 3 { v[2].parse().ok()? } else { 1 };
    if day < 1 || day > 31 {
        return None;
    }
    Some(Date { year, month, day })
}

impl Date {
    fn to_year_month(&self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    fn to_year_month_day(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

async fn calendar_data(
    Path(date): Path<String>,
    Extension(db): Extension<Database>,
) -> impl IntoResponse {
    tracing::info!("getting calendar_data: {}", date);
    let d = parse_date(&date);
    if d.is_none() {
        return (StatusCode::BAD_REQUEST, Json(Default::default()));
    }
    let d = d.unwrap();
    let conn = db.connection().expect("could not get connection");

    let mut qry = conn.prepare_cached("SELECT date, sum(calories * multiplier) as total FROM items WHERE date BETWEEN ?1 AND ?2 GROUP BY date").expect("could not prepare qry");
    let mut rows = qry
        .query(params![
            d.to_year_month(),
            Date {
                year: d.year,
                month: d.month + 1,
                day: 1
            }
            .to_year_month()
        ])
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

async fn add_weight(
    Json(weight): Json<AddWeight>,
    Extension(db): Extension<Database>,
) -> impl IntoResponse {
    tracing::info!("adding weight {:?}", &weight);
    let conn = db.connection().expect("could not get connection");
    conn.execute(
        "INSERT INTO weight (date, weight) VALUES (?1, ?2) 
        ON CONFLICT (date) DO UPDATE SET weight=?2;",
        params![weight.date, weight.weight],
    )
    .expect("could not insert weight into db");
    StatusCode::OK
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
            },
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

#[cfg(test)]
mod tests_date {
    use super::Date;
    #[test]
    fn test_to_year_month() {
        assert_eq!(
            Date {
                year: 2021,
                month: 1,
                day: 1
            }
            .to_year_month(),
            "2021-01"
        );
        assert_eq!(
            Date {
                year: 2021,
                month: 12,
                day: 1
            }
            .to_year_month(),
            "2021-12"
        );
    }

    #[test]
    fn test_to_year_month_day() {
        assert_eq!(
            Date {
                year: 2021,
                month: 1,
                day: 1
            }
            .to_year_month_day(),
            "2021-01-01"
        );
        assert_eq!(
            Date {
                year: 2021,
                month: 12,
                day: 1
            }
            .to_year_month_day(),
            "2021-12-01"
        );
        assert_eq!(
            Date {
                year: 2021,
                month: 1,
                day: 31
            }
            .to_year_month_day(),
            "2021-01-31"
        );
    }
}
