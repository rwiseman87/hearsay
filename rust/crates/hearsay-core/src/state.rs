//! Shared application state handed to every handler via axum's `State` extractor.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::Settings;
use crate::models::DownloadManager;
use hearsay_backends::archive::Sweeper;
use hearsay_engine::LiveEngine;

/// Per-process application state. Cheap to clone (a pool handle + `Arc`s).
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub settings: Arc<Settings>,
    pub session_token: Arc<String>,
    pub engine: Arc<dyn LiveEngine>,
    /// The notes-model catalog + the single active download (progress polled by the UI).
    pub downloads: Arc<DownloadManager>,
    /// Audio-archival progress + the failure memo, shared with the periodic sweep so the Settings
    /// button and the ticker cannot run two passes at once.
    pub archive: Arc<Sweeper>,
}

impl AppState {
    pub fn new(
        pool: SqlitePool,
        settings: Settings,
        session_token: String,
        engine: Arc<dyn LiveEngine>,
        archive: Arc<Sweeper>,
    ) -> Self {
        let downloads = Arc::new(DownloadManager::new(settings.models_dir.clone()));
        AppState {
            pool,
            settings: Arc::new(settings),
            session_token: Arc::new(session_token),
            engine,
            downloads,
            archive,
        }
    }
}
