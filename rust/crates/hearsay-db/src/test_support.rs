//! Shared fixtures for tests across the workspace, mirroring [`crate::MIGRATOR`]-backed production
//! setup so a test never disagrees with the real schema.

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use crate::{connect_options, MIGRATOR};

/// An in-memory pool with the real migrations applied.
///
/// Single-connection on purpose: every connection to `sqlite::memory:` gets its own private
/// database, so a larger pool would hand some queries an unmigrated one at random.
pub async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options("sqlite::memory:").expect("in-memory connect options"))
        .await
        .expect("open in-memory pool");
    MIGRATOR.run(&pool).await.expect("run migrations");
    pool
}
