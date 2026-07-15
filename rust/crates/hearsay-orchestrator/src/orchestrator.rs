//! [`Orchestrator`]: the [`LiveEngine`] implementation. Owns the single active meeting (Phase 1
//! records one at a time), serialized by an async op-lock; the sync accessors read the active
//! session behind a std mutex. Port of `hearsay.transcript.session.SessionManager`.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use uuid::Uuid;

use hearsay_db::models::{Meeting, MeetingStatus};
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
    /// Default recordings root when no `storage` UI override is stored; also the fallback root for
    /// locating legacy meetings (rows created before per-meeting `dir` pinning).
    output_dir: PathBuf,
    backend: Arc<dyn Backend>,
    /// Config defaults for the editable settings. The effective value at runtime is the stored
    /// `preferences` override else these — resolved from the DB at meeting start (`record`,
    /// `output_dir`) and stop (`auto_refine`, `recognition_threshold`), so a UI change takes effect
    /// on the next meeting without a restart.
    default_record: bool,
    default_auto_refine: bool,
    default_recognition_threshold: f64,
    /// The post-meeting refine. Wire it (via [`with_refiner`](Self::with_refiner)) so auto-refine is
    /// *available*; whether it actually runs at stop is gated by the effective `auto_refine` setting.
    /// `None` disables it entirely (the manual `/rediarize` route still drives the refine directly).
    refiner: Option<Arc<dyn Refiner>>,
    /// Serializes `start_meeting` / `stop_meeting` (so the busy-check and the set never race).
    op_lock: tokio::sync::Mutex<()>,
    /// The active session, readable by the sync `active_meeting` / `subscribe` accessors.
    active: Mutex<Option<ActiveSession>>,
    /// In-flight post-stop finalize tasks (refine + transcript write + status flip). A stop returns
    /// before its task completes; tracked so graceful shutdown and tests can await them.
    background: Mutex<Vec<JoinHandle<()>>>,
    /// Weak self-reference, set once via [`install_self`](Self::install_self) after the orchestrator
    /// is wrapped in an `Arc`. Lets the per-meeting capture-death supervisor call back into
    /// `stop_meeting` without a reference cycle. Empty (no supervisor finalize) until installed.
    self_weak: Mutex<Weak<Orchestrator>>,
}

impl Orchestrator {
    /// Build the orchestrator over a database pool, the default per-meeting output root, and the
    /// capture + transcription backend factory. Config defaults are record on / auto-refine on /
    /// recognition threshold 0.6 until overridden with [`with_defaults`](Self::with_defaults); the
    /// stored UI preferences override them per meeting.
    pub fn new(pool: SqlitePool, output_dir: PathBuf, backend: Arc<dyn Backend>) -> Self {
        Orchestrator {
            pool,
            output_dir,
            backend,
            default_record: true,
            default_auto_refine: true,
            default_recognition_threshold: 0.6,
            refiner: None,
            op_lock: tokio::sync::Mutex::new(()),
            active: Mutex::new(None),
            background: Mutex::new(Vec::new()),
            self_weak: Mutex::new(Weak::new()),
        }
    }

    /// Record the `Arc<Self>` handle so the per-meeting capture-death supervisor can finalize a
    /// meeting whose capture died. Call once, right after wrapping the orchestrator in an `Arc`
    /// (before serving). Without it, an unexpected capture death still winds the pipeline down but
    /// the meeting is not auto-finalized.
    pub fn install_self(self: &Arc<Self>) {
        *self.self_weak.lock().unwrap() = Arc::downgrade(self);
    }

    /// Set the config defaults for the editable settings (the values used when the UI has stored no
    /// override). Typically the resolved `Settings` (env/startup). The UI still overrides these per
    /// meeting via the `preferences` table.
    pub fn with_defaults(
        mut self,
        record: bool,
        auto_refine: bool,
        recognition_threshold: f64,
    ) -> Self {
        self.default_record = record;
        self.default_auto_refine = auto_refine;
        self.default_recognition_threshold = recognition_threshold;
        self
    }

    /// Make the post-meeting [`Refiner`] available so a meeting can auto-refine at stop (Python
    /// `SessionManager._maybe_auto_refine`). Whether it runs is decided per stop by the effective
    /// `auto_refine` setting. Without it, stop just finalizes; the manual `/rediarize` route drives
    /// the refine directly regardless.
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

        // Effective settings (stored UI override else the config default), resolved at start so a
        // change takes effect on the next meeting. Resolve them *before* the INSERT so the row is
        // created with its final `dir` in one statement (no INSERT-then-UPDATE window). The
        // recordings root is pinned onto the meeting so it stays locatable if Storage later changes.
        let output_root = queries::effective_output_dir(&self.pool, &self.output_dir).await?;
        let record = queries::effective_record(&self.pool, self.default_record).await?;
        let dir = output_root.join(&folder_name);
        let dir_str = dir.to_string_lossy().into_owned();

        let meeting =
            queries::create_meeting(&self.pool, &title, &folder_name, &dir_str, when).await?;

        // From here a failure must not strand the row at status=recording (it would render as a live
        // meeting forever). The row has no segments yet, so delete it (and clean up the empty folder)
        // before surfacing the error.
        match self.launch_pipeline(&meeting, &dir, record).await {
            Ok(()) => Ok(meeting),
            Err(err) => {
                let _ = queries::delete_meeting(&self.pool, meeting.id).await;
                let _ = tokio::fs::remove_dir(&dir).await;
                Err(err)
            }
        }
    }

    /// Create the recordings folder and start the capture + transcription pipeline, installing it as
    /// the active session. Split out so `start_meeting_inner` has one error path to clean up.
    async fn launch_pipeline(
        &self,
        meeting: &Meeting,
        dir: &Path,
        record: bool,
    ) -> Result<(), OrchestratorError> {
        tokio::fs::create_dir_all(dir).await?;
        let audio_path = record.then(|| dir.join("audio.wav"));
        let (pipeline, died_rx) = pipeline::spawn(
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
        self.spawn_capture_supervisor(meeting.id, died_rx);
        Ok(())
    }

    /// Watch for an unexpected capture death (helper crash / socket EOF): `died_rx` fires only then,
    /// not on an intentional stop (which drops the sender -> `Err`). On death, finalize the meeting
    /// through `stop_meeting` so it is not left falsely `recording`/active with a dead pipeline (and
    /// the closed broadcast channel tells live subscribers). A no-op if `install_self` was never
    /// called (the weak ref is empty).
    fn spawn_capture_supervisor(
        &self,
        meeting_id: Uuid,
        died_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        let weak = self.self_weak.lock().unwrap().clone();
        tokio::spawn(async move {
            if died_rx.await.is_err() {
                return; // intentional stop: nothing to do
            }
            if let Some(orch) = weak.upgrade() {
                tracing::warn!(meeting = %meeting_id, "capture died; finalizing meeting");
                if let Err(err) = orch.stop_meeting(meeting_id).await {
                    tracing::warn!(error = ?err, meeting = %meeting_id, "failed to finalize meeting after capture died");
                }
            }
        });
    }

    /// Resolve whether a stop should auto-refine this meeting (Python `_maybe_auto_refine`'s gate):
    /// a [`Refiner`] must be wired, the effective `auto_refine` setting on, and a recorded
    /// `audio.wav` present. Returns the refiner + the effective recognition threshold when it
    /// should; `None` (with the reason logged) when the live finals should stand as the transcript.
    /// Read at stop so a UI toggle takes effect on the next meeting.
    async fn should_auto_refine(&self, meeting: &Meeting) -> Option<(Arc<dyn Refiner>, f64)> {
        let refiner = self.refiner.clone()?;
        let (auto_refine, threshold) = match queries::effective_speakers(
            &self.pool,
            self.default_auto_refine,
            self.default_recognition_threshold,
        )
        .await
        {
            Ok(values) => values,
            Err(err) => {
                tracing::warn!(error = %err, "auto-refine: failed to read settings; keeping live segments");
                return None;
            }
        };
        if !auto_refine {
            tracing::debug!(meeting = %meeting.id, "auto-refine disabled by settings; keeping live segments");
            return None;
        }
        let audio = meeting.dir_path(&self.output_dir).join("audio.wav");
        if !audio.exists() {
            tracing::debug!(meeting = %meeting.id, "auto-refine skipped: no recorded audio");
            return None;
        }
        Some((refiner, threshold))
    }

    /// Track an in-flight post-stop finalize task, pruning any already-finished handles.
    fn track_background(&self, handle: JoinHandle<()>) {
        let mut background = self.background.lock().unwrap();
        background.retain(|h| !h.is_finished());
        background.push(handle);
    }

    /// Await every in-flight post-stop finalize task (refine + transcript write + status flip).
    /// For graceful shutdown and for tests that assert on the refined result.
    pub async fn wait_for_refines(&self) {
        let handles: Vec<_> = std::mem::take(&mut *self.background.lock().unwrap());
        for handle in handles {
            let _ = handle.await;
        }
    }
}

/// Re-diarize + re-transcribe the meeting's Them track and replace its live segments (best-effort:
/// a refine or persist error is logged, leaving the live finals in place). The settings gate +
/// audio-existence check already passed in [`Orchestrator::should_auto_refine`].
async fn run_auto_refine(
    pool: &SqlitePool,
    output_dir: &Path,
    refiner: &Arc<dyn Refiner>,
    threshold: f64,
    meeting: &Meeting,
) {
    let audio = meeting.dir_path(output_dir).join("audio.wav");
    match refiner.refine(&audio).await {
        Ok(result) => {
            let count = result.segments.len();
            match queries::replace_them_segments(pool, meeting.id, &result, threshold).await {
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
        Err(err) => tracing::warn!(error = %err, "auto-refine failed; keeping live segments"),
    }
}

/// Write the meeting's `transcript.md` + `meeting.json` from its finalized segments. Best-effort:
/// a read/write failure is logged, never surfaced (the stop already succeeded).
async fn write_transcript(pool: &SqlitePool, output_dir: &Path, meeting: &Meeting) {
    match queries::list_segments(pool, meeting.id).await {
        Ok(segments) => {
            let dir = meeting.dir_path(output_dir);
            let meeting = meeting.clone();
            let write = tokio::task::spawn_blocking(move || {
                crate::markdown::write_meeting_files(&dir, &meeting, &segments)
            })
            .await;
            match write {
                Ok(Err(err)) => {
                    tracing::error!(error = %err, "failed to write meeting transcript files")
                }
                Err(err) => tracing::error!(error = %err, "transcript writer panicked"),
                Ok(Ok(())) => {}
            }
        }
        Err(err) => tracing::error!(error = %err, "failed to read segments for transcript"),
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
        // Critical section (op-lock): take + close the active pipeline, verify the row, and finalize
        // it. The refine + transcript write are deliberately *outside* the lock (a tracked
        // background task) so a hung refine cannot wedge the lifecycle and a new meeting can start
        // while this one refines.
        let (meeting, refine) = {
            let _op = self.op_lock.lock().await;
            // Take + close the active session if it is this meeting (stopping a non-active meeting
            // id still finalizes its row, matching the Python SessionManager).
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
                // The meeting's sidecars are closed, so the compute (ANE) is free — (re)warm the pool
                // for the next meeting now, rather than while this one was recording (which starves
                // the warm load and can leave a dead pair wedging the "Start" gate).
                self.backend.ensure_pool_warm();
            }

            let Some(existing) = queries::get_meeting(&self.pool, meeting_id)
                .await
                .map_err(OrchestratorError::from)?
            else {
                return Ok(None);
            };
            // Double-stop guard: a meeting already past `recording` (mid-refine or finalized) is not
            // re-finalized or re-refined — return its current row unchanged, so a duplicate stop
            // never rewrites `ended_at` or launches a second refine.
            if existing.status != MeetingStatus::Recording {
                return Ok(Some(existing));
            }

            // Decide whether this stop will auto-refine; that picks the interim status the UI sees
            // (`refining` while the background task runs, else straight to `finalized`).
            let refine = self.should_auto_refine(&existing).await;
            let status = if refine.is_some() {
                MeetingStatus::Refining
            } else {
                MeetingStatus::Finalized
            };
            queries::finalize_meeting(&self.pool, meeting_id, Utc::now(), status)
                .await
                .map_err(OrchestratorError::from)?;
            let meeting = queries::get_meeting(&self.pool, meeting_id)
                .await
                .map_err(OrchestratorError::from)?;
            (meeting, refine)
        };

        let Some(meeting) = meeting else {
            return Ok(None);
        };

        // Post-lock: refine (if resolved) -> rewrite the transcript -> flip `refining` to
        // `finalized`. Runs on a tracked background task so stop returns promptly with the op-lock
        // free. The manual `/rediarize` route drives the same refine when auto-refine is off.
        let pool = self.pool.clone();
        let output_dir = self.output_dir.clone();
        let task_meeting = meeting.clone();
        let handle = tokio::spawn(async move {
            if let Some((refiner, threshold)) = refine {
                run_auto_refine(&pool, &output_dir, &refiner, threshold, &task_meeting).await;
            }
            write_transcript(&pool, &output_dir, &task_meeting).await;
            if task_meeting.status == MeetingStatus::Refining {
                if let Err(err) = queries::set_meeting_finalized(&pool, task_meeting.id).await {
                    tracing::warn!(
                        error = %err,
                        meeting = %task_meeting.id,
                        "failed to mark meeting finalized after refine"
                    );
                }
            }
        });
        self.track_background(handle);

        Ok(Some(meeting))
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

    fn transcription_warming(&self, meeting_id: Uuid) -> Option<bool> {
        let guard = self.active.lock().unwrap();
        match guard.as_ref() {
            Some(s) if s.meeting_id == meeting_id => {
                Some(s.pipeline.warming.load(Ordering::SeqCst))
            }
            _ => None,
        }
    }

    fn sidecars_ready(&self) -> bool {
        // While idle (no meeting running), make sure the pool is warming — this both starts the
        // first warm and recovers a pair that died while idle (e.g. lost an ANE race to the auto-
        // refine), so the "Start" gate can never wedge on a dead/empty pool with nothing to re-trigger
        // it. A no-op once a healthy pair is loading/ready. Skipped while a meeting is active: warming
        // then would starve the live sidecars on the compute (ANE).
        if self.active_meeting().is_none() {
            self.backend.ensure_pool_warm();
        }
        self.backend.sidecars_ready()
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
