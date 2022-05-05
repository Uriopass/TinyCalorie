use crate::db::Database;
use anyhow::{bail, Context, Result};
use include_dir::Dir;
use r2d2_sqlite::rusqlite::{Connection, ErrorCode};

fn get_migrated(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = match conn.prepare("SELECT migration FROM sqlx_pg_migrate ORDER BY id;") {
        Ok(x) => x,
        Err(r2d2_sqlite::rusqlite::Error::SqliteFailure(sqlite_err, msg)) => {
            if sqlite_err.code == ErrorCode::Unknown
                && msg
                    .as_ref()
                    .map(|v| v.starts_with("no such table"))
                    .unwrap_or(false)
            {
                return Ok(vec![]);
            } else {
                bail!("{:?}", msg.unwrap_or_default());
            }
        }
        Err(e) => Err(e)?,
    };
    let migrated = stmt.query_map([], |row| row.get("migration"))?;
    migrated
        .map(|x| x.map_err(|e| anyhow::Error::from(e)))
        .collect()
}

/// Runs the migrations contained in the directory. See module documentation for
/// more information.
pub fn migrate(db: &Database, dir: &Dir<'_>) -> Result<()> {
    tracing::info!("running migrations");

    let mut client = db.connection()?;
    let migrated = get_migrated(&client).context("error getting migrations")?;
    tracing::info!("got existing migrations from table");

    let tx = client.transaction().context("error creating transaction")?;
    if migrated.is_empty() {
        tracing::info!("no migration table, creating it");

        tx.execute_batch(
            r#"
                CREATE TABLE IF NOT EXISTS sqlx_pg_migrate (
                    id SERIAL PRIMARY KEY,
                    migration TEXT UNIQUE,
                    created TIMESTAMP NOT NULL DEFAULT current_timestamp
                );
            "#,
        )
        .context("error creating migration table")?;
    }
    let mut files: Vec<_> = dir.files().collect();
    if migrated.len() > files.len() {
        bail!("some migrations were deleted")
    }
    files.sort_by(|a, b| a.path().partial_cmp(b.path()).unwrap());
    for (pos, f) in files.iter().enumerate() {
        let path = f.path().to_str().context("invalid path")?;

        if pos < migrated.len() {
            if migrated[pos] != path {
                bail!("migration is missing: {}", path)
            }
            continue;
        }
        tracing::info!("running and inserting migration: {}", path);

        let content = f.contents_utf8().context("invalid file content")?;
        tx.execute_batch(content)?;
        tx.execute(
            "INSERT INTO sqlx_pg_migrate (migration) VALUES ($1)",
            [&path],
        )
        .context("error running transaction")?;
    }
    tx.commit().context("error commiting transaction")?;
    tracing::info!("sucessfully ran migrations");
    Ok(())
}
