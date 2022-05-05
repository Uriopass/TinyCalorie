use axum::Extension;
use r2d2::{Error, Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;

#[derive(Clone)]
pub struct Database {
    pool: Pool<SqliteConnectionManager>,
}

impl Database {
    pub fn new(path: &str) -> Result<Extension<Self>, Error> {
        let manager = SqliteConnectionManager::file(path);
        let pool = Pool::new(manager)?;
        Ok(Extension(Self { pool }))
    }

    pub fn connection(&self) -> Result<PooledConnection<SqliteConnectionManager>, Error> {
        Ok(self.pool.get()?)
    }
}
