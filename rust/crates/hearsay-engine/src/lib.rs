//! The live-capture seam — the neutral port shared by the API and the orchestrator.
//!
//! The meeting *lifecycle* (start / stop) and the live transcript stream require a running capture
//! and inference pipeline. `hearsay-core` (the HTTP/WS API) depends on this [`LiveEngine`] trait,
//! and `hearsay-orchestrator` implements it — exactly as the Python `create_app` takes an injected
//! `SessionManager`. Keeping the trait here (not in either crate) lets the core consume it and the
//! orchestrator implement it without a dependency cycle, and keeps the orchestrator off the web
//! stack. Until the orchestrator is wired into the binary, [`DisabledEngine`] answers those routes
//! with 503 / a clean WebSocket close; every read + pure-DB-write + serving route works without it.

use async_trait::async_trait;
use tokio::sync::broadcast;
use uuid::Uuid;

use hearsay_db::models::Meeting;

/// Why a live operation could not be performed.
#[derive(Debug)]
pub enum LiveError {
    /// The capture / inference engine is not wired in this build (503).
    Unavailable,
    /// A meeting is already recording (409).
    Busy(String),
    /// An unexpected failure in the engine (e.g. a database or spawn error) (500).
    Internal(String),
}

/// Drives the live capture + inference pipeline behind the core's lifecycle routes.
#[async_trait]
pub trait LiveEngine: Send + Sync {
    /// Start a meeting: create its folder + row and begin capture. `title` defaults when `None`.
    async fn start_meeting(&self, title: Option<String>) -> Result<Meeting, LiveError>;

    /// Stop a meeting: finalize it and run the post-meeting refine. `None` if it does not exist.
    async fn stop_meeting(&self, meeting_id: Uuid) -> Result<Option<Meeting>, LiveError>;

    /// The currently recording meeting, if any.
    fn active_meeting(&self) -> Option<Uuid>;

    /// Subscribe to a meeting's live transcript events (JSON lines). `None` when that meeting is
    /// not the active recording session.
    fn subscribe(&self, meeting_id: Uuid) -> Option<broadcast::Receiver<String>>;

    /// Whether the active meeting's transcription sidecars are still loading their models, so the
    /// live WebSocket can send a warm-up snapshot to a new subscriber. `Some(true)` = still loading
    /// (a cold start), `Some(false)` = serving, `None` = not the active session. Defaults to `None`
    /// for engines without live capture.
    fn transcription_warming(&self, _meeting_id: Uuid) -> Option<bool> {
        None
    }

    /// Whether the pre-warmed transcription sidecars for the *next* meeting have finished loading
    /// their models and can transcribe immediately. Lets the UI gate "Start" until a meeting can
    /// actually be used (rather than starting one that shows nothing for ~30 s while models load).
    /// Defaults to `true` for engines without a warm pool (they impose no such wait).
    fn sidecars_ready(&self) -> bool {
        true
    }
}

/// The placeholder engine used until `hearsay-orchestrator` is wired in: no capture, no active
/// session. Lifecycle routes return 503; the live WebSocket accepts then closes cleanly.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledEngine;

#[async_trait]
impl LiveEngine for DisabledEngine {
    async fn start_meeting(&self, _title: Option<String>) -> Result<Meeting, LiveError> {
        Err(LiveError::Unavailable)
    }

    async fn stop_meeting(&self, _meeting_id: Uuid) -> Result<Option<Meeting>, LiveError> {
        Err(LiveError::Unavailable)
    }

    fn active_meeting(&self) -> Option<Uuid> {
        None
    }

    fn subscribe(&self, _meeting_id: Uuid) -> Option<broadcast::Receiver<String>> {
        None
    }
}
