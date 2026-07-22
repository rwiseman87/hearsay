//! The live-capture seam — the neutral port shared by the API and the orchestrator.
//!
//! The meeting *lifecycle* (start / stop) and the live transcript stream require a running capture
//! and inference pipeline. `hearsay-core` (the HTTP/WS API) depends on this [`LiveEngine`] trait,
//! and `hearsay-orchestrator` implements it — the core takes the engine as an injected dependency.
//! Keeping the trait here (not in either crate) lets the core consume it and the
//! orchestrator implement it without a dependency cycle, and keeps the orchestrator off the web
//! stack. When no orchestrator is wired (tests, or a capture-less build), [`DisabledEngine`] answers those routes
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

    /// Re-run the offline refine for a stored meeting: re-diarize + re-transcribe its recorded Them
    /// track and replace the stored Them segments (and rewrite the transcript) in place. A meeting
    /// whose track has no remote speech is a no-op that keeps the existing segments. Drives the
    /// manual "Refine speakers" route with the same refine path as the auto-refine at stop.
    /// [`LiveError::Unavailable`] when no capture/inference engine is wired.
    async fn rediarize(&self, meeting_id: Uuid) -> Result<(), LiveError>;

    /// Generate (or regenerate) the meeting's notes — a summary + action items — from its finalized
    /// transcript with the local LLM, persist them, and write `notes.md`. Re-runnable on demand
    /// (the "Generate notes" route) independent of re-diarization, and run automatically at stop when
    /// the setting is on. [`LiveError::Unavailable`] when no summarizer/model is wired or the meeting
    /// has no transcript to summarize.
    async fn generate_notes(&self, meeting_id: Uuid) -> Result<(), LiveError>;

    /// Whether the active meeting's transcription sidecars are still loading their models, so the
    /// live WebSocket can send a warm-up snapshot to a new subscriber. `Some(true)` = still loading
    /// (a cold start), `Some(false)` = serving, `None` = not the active session. Defaults to `None`
    /// for engines without live capture.
    fn transcription_warming(&self, _meeting_id: Uuid) -> Option<bool> {
        None
    }

    /// Reset the active meeting's inactivity clock — the "Keep recording" action, which tells the
    /// engine the user is present so a silent-but-attended meeting is not nudged or auto-ended. A
    /// no-op unless `meeting_id` is the active recording session. Default no-op for engines without
    /// live capture.
    fn keep_alive(&self, _meeting_id: Uuid) {}

    /// The active meeting's inactivity-prompt state, so the live WebSocket can snapshot it to a new
    /// subscriber (a user reopening the window mid-silence). `Some(silent_seconds)` while a prompt is
    /// active, `None` otherwise or when `meeting_id` is not the active session. Default `None` for
    /// engines without live capture.
    fn inactivity_prompt(&self, _meeting_id: Uuid) -> Option<u64> {
        None
    }

    /// Whether the pre-warmed transcription sidecars for the *next* meeting have finished loading
    /// their models and can transcribe immediately. Lets the UI gate "Start" until a meeting can
    /// actually be used (rather than starting one that shows nothing for ~30 s while models load).
    /// Defaults to `true` for engines without a warm pool (they impose no such wait).
    fn sidecars_ready(&self) -> bool {
        true
    }

    /// Re-render a meeting's on-disk exports (`transcript.md` + `meeting.json`, and `notes.md` when it
    /// has notes) from the current DB rows. Called best-effort after an in-app edit so the exported
    /// files stay in step with the edited transcript/notes; the DB remains the source of truth
    /// regardless. Default no-op for engines without an export path (e.g. [`DisabledEngine`]).
    async fn export_meeting(&self, _meeting_id: Uuid) -> Result<(), LiveError> {
        Ok(())
    }

    /// Best-effort write of a meeting's `my-notes.md` (the user-authored "My notes" body) from the
    /// stored `user_notes` row. Called after an autosave so the on-disk export stays in step with the
    /// DB; unlike [`export_meeting`] it touches only `my-notes.md`, so it is safe to call while the
    /// meeting is still recording (it never rewrites the in-progress transcript). Default no-op for
    /// engines without an export path.
    async fn export_user_notes(&self, _meeting_id: Uuid) -> Result<(), LiveError> {
        Ok(())
    }

    /// Await any in-flight background work (e.g. a post-stop refine + transcript write) so a graceful
    /// shutdown does not cut one off mid-write. Default no-op for engines with no background tasks.
    async fn shutdown(&self) {}
}

/// The placeholder engine used when no `hearsay-orchestrator` is wired in: no capture, no active
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

    async fn rediarize(&self, _meeting_id: Uuid) -> Result<(), LiveError> {
        Err(LiveError::Unavailable)
    }

    async fn generate_notes(&self, _meeting_id: Uuid) -> Result<(), LiveError> {
        Err(LiveError::Unavailable)
    }
}
