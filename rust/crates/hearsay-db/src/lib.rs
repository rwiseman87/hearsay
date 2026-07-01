//! Persistence: SQLite via SQLx (WAL + `busy_timeout` + foreign keys), UUID primary keys,
//! `created_at` / `updated_at` timestamps, forward-only migrations. Portable to PostgreSQL later.
//!
//! Port of `src/hearsay/models/` and `src/hearsay/db/`. Queries are runtime-checked
//! (`sqlx::query`/`query_as`); the compile-time `query!` macros (offline `.sqlx` cache) are a
//! future upgrade. See `docs/architecture-cross-platform.md`.

pub mod models;
pub mod queries;

use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

/// Embedded forward-only migrations (`migrations/`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Connection options with the pragmas the Python engine sets: WAL journal, a 5s busy timeout,
/// and enforced foreign keys.
pub fn connect_options(url: &str) -> Result<SqliteConnectOptions, sqlx::Error> {
    Ok(SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true))
}

/// Open a connection pool (WAL + busy-timeout + foreign-keys pragmas, matching the Python engine
/// config) and apply any pending migrations.
pub async fn connect(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(connect_options(url)?)
        .await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}
