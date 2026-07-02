//! [`Orchestrator`]: the [`LiveEngine`] implementation. Owns the single active meeting (Phase 1
//! records one at a time), serialized by an async op-lock; the sync accessors read the active
//! session behind a std mutex. Port of `hearsay.transcript.session.SessionManager`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use uuid::Uuid;

use hearsay_db::models::Meeting;
use hearsay_db::queries;
use hearsay_engine::{LiveEngine, LiveError};

use crate::error::OrchestratorError;
use crate::pipeline::{self, Pipeline};
use crate::traits::{Backend, Refiner};

/// The single active recording session: its meeting id and the running pipeline.
struct ActiveSession {
    meeting_id: Uuid,
    pipeline: Pipeline,
}

/// Drives the meeting lifecycle + live transcript broadcast behind `hearsay-core`'s routes.
pub struct Orchestrator {
    pool: SqlitePool,
    output_dir: PathBuf,
    backend: Arc<dyn Backend>,
    /// Record the stereo `audio.wav` per meeting (for playback + the offline refine). Default on.
    record_audio: bool,
    /// The post-meeting refine, run at stop when set (auto-refine). `None` disables it (the manual
    /// `/rediarize` route still works — it drives the refine directly).
    refiner: Option<Arc<dyn Refiner>>,
    /// Serializes `start_meeting` / `stop_meeting` (so the busy-check and the set never race).
    op_lock: tokio::sync::Mutex<()>,
    /// The active session, readable by the sync `active_meeting` / `subscribe` accessors.
    active: Mutex<Option<ActiveSession>>,
}

impl Orchestrator {
    /// Build the orchestrator over a database pool, the per-meeting output root, and the capture +
    /// transcription backend factory. Records `audio.wav` by default (see
    /// [`with_audio_recording`](Self::with_audio_recording)).
    pub fn new(pool: SqlitePool, output_dir: PathBuf, backend: Arc<dyn Backend>) -> Self {
        Orchestrator {
            pool,
            output_dir,
            backend,
            record_audio: true,
            refiner: None,
            op_lock: tokio::sync::Mutex::new(()),
            active: Mutex::new(None),
        }
    }

    /// Set whether to record the per-meeting `audio.wav` (the retention toggle; the eventual config
    /// wires `audio.record` here).
    pub fn with_audio_recording(mut self, record: bool) -> Self {
        self.record_audio = record;
        self
    }

    /// Wire the post-meeting [`Refiner`] so a meeting auto-refines at stop (Python
    /// `SessionManager._maybe_auto_refine`). Without it, stop just finalizes; the manual
    /// `/rediarize` route drives the refine directly.
    pub fn with_refiner(mut self, refiner: Arc<dyn Refiner>) -> Self {
        self.refiner = Some(refiner);
        self
    }

    async fn start_meeting_inner(
        &self,
        title: Option<String>,
    ) -> Result<Meeting, OrchestratorError> {
        let when = Utc::now();
        let title = title.unwrap_or_else(|| default_title(when));
        let folder_name = meeting_folder_name(&title, when);

        let meeting = queries::create_meeting(&self.pool, &title, &folder_name, when).await?;
        let folder = self.output_dir.join(&folder_name);
        tokio::fs::create_dir_all(&folder).await?;

        let audio_path = self.record_audio.then(|| folder.join("audio.wav"));
        let pipeline = pipeline::spawn(
            self.backend.build(),
            self.pool.clone(),
            meeting.id,
            audio_path,
        )
        .await?;
        *self.active.lock().unwrap() = Some(ActiveSession {
            meeting_id: meeting.id,
            pipeline,
        });
        Ok(meeting)
    }

    /// Write the meeting's `transcript.md` + `meeting.json` from its finalized segments. Best-effort:
    /// a read/write failure is logged, never surfaced (the stop already succeeded).
    async fn write_transcript(&self, meeting: &Meeting) {
        match queries::list_segments(&self.pool, meeting.id).await {
            Ok(segments) => {
                let dir = self.output_dir.join(&meeting.folder);
                if let Err(err) = crate::markdown::write_meeting_files(&dir, meeting, &segments) {
                    tracing::error!(error = %err, "failed to write meeting transcript files");
                }
            }
            Err(err) => tracing::error!(error = %err, "failed to read segments for transcript"),
        }
    }

    /// Best-effort post-meeting refine (Python `_maybe_auto_refine`): if a [`Refiner`] is wired and
    /// the meeting recorded an `audio.wav`, re-diarize + re-transcribe the Them track and replace
    /// its live segments. Never fails the stop — a missing recording (`audio.record` off) or a
    /// refine error is logged and skipped, leaving the live finals as the transcript.
    async fn maybe_auto_refine(&self, meeting: &Meeting) {
        let Some(refiner) = self.refiner.as_ref() else {
            return;
        };
        let audio = self.output_dir.join(&meeting.folder).join("audio.wav");
        if !audio.exists() {
            tracing::debug!(meeting = %meeting.id, "auto-refine skipped: no recorded audio");
            return;
        }
        match refiner.refine(&audio).await {
            Ok(result) => {
                let count = result.segments.len();
                match queries::replace_them_segments(&self.pool, meeting.id, &result).await {
                    Ok(()) => tracing::info!(
                        meeting = %meeting.id,
                        segments = count,
                        "auto-refined Them segments at stop"
                    ),
                    Err(err) => tracing::warn!(
                        error = %err,
                        "auto-refine: failed to persist refined segments; keeping live segments"
                    ),
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "auto-refine failed; keeping live segments");
            }
        }
    }
}

#[async_trait]
impl LiveEngine for Orchestrator {
    async fn start_meeting(&self, title: Option<String>) -> Result<Meeting, LiveError> {
        let _op = self.op_lock.lock().await;
        if self.active.lock().unwrap().is_some() {
            return Err(LiveError::Busy("a meeting is already recording".into()));
        }
        Ok(self.start_meeting_inner(title).await?)
    }

    async fn stop_meeting(&self, meeting_id: Uuid) -> Result<Option<Meeting>, LiveError> {
        let _op = self.op_lock.lock().await;
        // Take + close the active session if it is this meeting (stopping a non-active meeting id
        // still finalizes its row, matching the Python SessionManager).
        let session = {
            let mut guard = self.active.lock().unwrap();
            if guard.as_ref().is_some_and(|s| s.meeting_id == meeting_id) {
                guard.take()
            } else {
                None
            }
        };
        if let Some(session) = session {
            session.pipeline.close().await;
        }

        if queries::get_meeting(&self.pool, meeting_id)
            .await
            .map_err(OrchestratorError::from)?
            .is_none()
        {
            return Ok(None);
        }
        queries::finalize_meeting(&self.pool, meeting_id, Utc::now())
            .await
            .map_err(OrchestratorError::from)?;
        let finalized = queries::get_meeting(&self.pool, meeting_id)
            .await
            .map_err(OrchestratorError::from)?;
        if let Some(meeting) = &finalized {
            // Auto-refine (best-effort) before writing the transcript so it reflects the refined
            // speakers; the manual `/rediarize` route drives the same refine when auto is off.
            self.maybe_auto_refine(meeting).await;
            self.write_transcript(meeting).await;
        }
        Ok(finalized)
    }

    fn active_meeting(&self) -> Option<Uuid> {
        self.active.lock().unwrap().as_ref().map(|s| s.meeting_id)
    }

    fn subscribe(&self, meeting_id: Uuid) -> Option<broadcast::Receiver<String>> {
        let guard = self.active.lock().unwrap();
        match guard.as_ref() {
            Some(s) if s.meeting_id == meeting_id => Some(s.pipeline.broadcast_tx.subscribe()),
            _ => None,
        }
    }
}

/// Default meeting title when the caller does not supply one (matches Python `_default_title`).
fn default_title(when: DateTime<Utc>) -> String {
    format!("Meeting {}", when.format("%Y-%m-%d %H:%M"))
}

/// `<YYYY-MM-DD_HHMM>_<slug>` — the per-meeting on-disk folder name (matches Python
/// `meeting_folder_name`).
fn meeting_folder_name(title: &str, when: DateTime<Utc>) -> String {
    format!("{}_{}", when.format("%Y-%m-%d_%H%M"), slugify(title))
}

/// Lowercase, collapse every run of non-`[a-z0-9]` to a single `-`, trim `-`; empty -> `"meeting"`.
/// Matches Python `slugify` (`re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")`).
fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "meeting".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_python_semantics() {
        assert_eq!(slugify("Standup: Q3 Planning!"), "standup-q3-planning");
        assert_eq!(slugify("  hello  world  "), "hello-world");
        assert_eq!(slugify("Team--Sync"), "team-sync");
        assert_eq!(slugify("!!!"), "meeting");
        assert_eq!(slugify(""), "meeting");
        assert_eq!(slugify("café"), "caf");
    }

    #[test]
    fn folder_name_is_timestamp_then_slug() {
        let when = DateTime::parse_from_rfc3339("2026-07-02T09:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            meeting_folder_name("Weekly Sync", when),
            "2026-07-02_0905_weekly-sync"
        );
        assert_eq!(default_title(when), "Meeting 2026-07-02 09:05");
    }
}
