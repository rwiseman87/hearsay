//! The live-capture seam.
//!
//! The meeting *lifecycle* (start / stop) and the live transcript stream require a running capture
//! and inference pipeline — the job of `hearsay-orchestrator` (and, under it, `hearsay-capture` +
//! `hearsay-inference`), which are not built yet. The core depends on this trait, not on those
//! crates, exactly as the Python `create_app` takes an injected `SessionManager`. Until the
//! orchestrator lands, [`DisabledEngine`] answers those routes with 503 / a clean WebSocket close;
//! every read + pure-DB-write + serving route works without it.

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
}

/// The placeholder engine used until `hearsay-orchestrator` is built: no capture, no active
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
