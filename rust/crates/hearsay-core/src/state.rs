//! Shared application state handed to every handler via axum's `State` extractor.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::Settings;
use crate::engine::LiveEngine;

/// Per-process application state. Cheap to clone (a pool handle + `Arc`s).
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub settings: Arc<Settings>,
    pub session_token: Arc<String>,
    pub engine: Arc<dyn LiveEngine>,
}

impl AppState {
    pub fn new(
        pool: SqlitePool,
        settings: Settings,
        session_token: String,
        engine: Arc<dyn LiveEngine>,
    ) -> Self {
        AppState {
            pool,
            settings: Arc::new(settings),
            session_token: Arc::new(session_token),
            engine,
        }
    }
}
