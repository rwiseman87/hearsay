//! Persistence: SQLite via SQLx/SeaORM (WAL + `busy_timeout`), UUID primary keys,
//! `created_at`/`updated_at` timestamps, forward-only migrations. Portable to PostgreSQL later.
//!
//! Rust port of `src/hearsay/models/` and `src/hearsay/db/`.
//!
//! Scaffold: see `docs/architecture-cross-platform.md`.
