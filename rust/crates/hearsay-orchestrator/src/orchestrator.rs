//! [`Orchestrator`]: the [`LiveEngine`] implementation. Owns the single active meeting (Phase 1
//! records one at a time), serialized by an async op-lock; the sync accessors read the active
//! session behind a std mutex. Port of `hearsay.transcript.session.SessionManager`.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

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

/// How often the background warm ticker re-checks the sidecar pool while idle. The check is cheap
/// and idempotent when a healthy pair is present; on this cadence it re-spawns a warm pair that died
/// while idle, so the "Start" gate can never wedge on an empty/dead pool.
const WARM_TICK_INTERVAL: Duration = Duration::from_secs(2);

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
    /// One-permit gate serializing ANE-heavy work (P1): a live meeting holds it for its whole
    /// duration (acquired off the start path inside its pipeline) and the offline refine takes it for
    /// each run, so refine and live capture never run on the ANE at once. Always released, so it is
    /// deadlock-free: the live side releases at stop, the refine when it returns.
    ane_gate: Arc<tokio::sync::Semaphore>,
    /// Serializes `start_meeting` / `stop_meeting` (so the busy-check and the set never race).
    op_lock: tokio::sync::Mutex<()>,
    /// The active session, readable by the sync `active_meeting` / `subscribe` accessors.
    active: Mutex<Option<ActiveSession>>,
    /// In-flight post-stop finalize tasks (refine + transcript write + status flip). A stop returns
    /// before its task completes; tracked so graceful shutdown and tests can await them.
    background: Mutex<Vec<JoinHandle<()>>>,
    /// The background warm ticker (P3): re-warms the sidecar pool while idle, off the polled
    /// `sidecars_ready` read. Set once by [`spawn_warm_ticker`](Self::spawn_warm_ticker); tracked so
    /// it is aborted when the orchestrator drops.
    warm_ticker: OnceLock<JoinHandle<()>>,
    /// Weak self-reference, always set at construction via [`into_arc`](Self::into_arc) (with
    /// `Arc::new_cyclic`), so the per-meeting capture-death supervisor can call back into
    /// `stop_meeting` without a reference cycle — and it can never be forgotten. Empty only when the
    /// orchestrator was built with [`new`](Self::new) alone (unit tests that never drive a capture
    /// death), where the supervisor simply no-ops.
    self_weak: Weak<Orchestrator>,
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
            ane_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            op_lock: tokio::sync::Mutex::new(()),
            active: Mutex::new(None),
            background: Mutex::new(Vec::new()),
            warm_ticker: OnceLock::new(),
            self_weak: Weak::new(),
        }
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

    /// Wrap the orchestrator in an `Arc`, wiring its weak self-reference in the *same* step via
    /// [`Arc::new_cyclic`] so the capture-death supervisor's `weak.upgrade()` is always live — there
    /// is no separate init call to forget (P4). Apply [`with_defaults`](Self::with_defaults) /
    /// [`with_refiner`](Self::with_refiner) before this; start the warm ticker with
    /// [`spawn_warm_ticker`](Self::spawn_warm_ticker) after.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new_cyclic(move |weak| {
            let mut orch = self;
            orch.self_weak = weak.clone();
            orch
        })
    }

    /// Start the background warm ticker (P3): on a fixed cadence, re-warm the sidecar pool for the
    /// next meeting, but only when the compute (ANE) is free — never while a meeting is active or the
    /// shared [`ane_gate`](Self::ane_gate) is held by a background refine. This keeps warm *recovery*
    /// off the polled [`sidecars_ready`](LiveEngine::sidecars_ready) read (now a pure query) while
    /// still re-warming a pair that died idle. The task holds a [`Weak`] self-ref (so it stops once
    /// the orchestrator is dropped) and is tracked so it is aborted on drop. Call once, from within
    /// the Tokio runtime, on the `Arc` returned by [`into_arc`](Self::into_arc).
    pub fn spawn_warm_ticker(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(WARM_TICK_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(orch) = weak.upgrade() else { return };
                orch.maybe_warm_pool();
            }
        });
        let _ = self.warm_ticker.set(handle);
    }

    /// Re-warm the sidecar pool for the next meeting only when the ANE is free: never while a meeting
    /// is active or starting (its live sidecars own the ANE) and never while a background refine
    /// holds the shared [`ane_gate`](Self::ane_gate) (warming then would contend with the very work
    /// the gate serializes). Called off the read path by the warm ticker, so a `sidecars_ready` poll
    /// never spawns a process.
    fn maybe_warm_pool(&self) {
        if self.active_meeting().is_some() {
            return;
        }
        if self.ane_gate.available_permits() == 0 {
            return;
        }
        self.backend.ensure_pool_warm();
    }

    async fn start_meeting_inner(
        &self,
        title: Option<String>,
    ) -> Result<Meeting, OrchestratorError> {
        let when = Utc::now();
        let title = title.unwrap_or_else(|| default_title(when));
        let base_folder = meeting_folder_name(&title, when);

        // Effective settings (stored UI override else the config default), resolved at start so a
        // change takes effect on the next meeting. Resolve them *before* the INSERT so the row is
        // created with its final `dir` in one statement (no INSERT-then-UPDATE window). The
        // recordings root is pinned onto the meeting so it stays locatable if Storage later changes.
        let output_root = queries::effective_output_dir(&self.pool, &self.output_dir).await?;
        let record = queries::effective_record(&self.pool, self.default_record).await?;
        // Folder names have minute resolution, so two same-title meetings within one minute would
        // otherwise resolve to one shared directory that a later delete would wipe. Resolve the first
        // free `<base>`, `<base>-2`, … under the effective root; `start_meeting` is op-lock-serialized
        // so this check-then-create can't race in-process, and the `dir` UNIQUE index (0004) is the
        // cross-process backstop.
        let (folder_name, dir) = unique_meeting_dir(&output_root, &base_folder);
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
            self.ane_gate.clone(),
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
    /// the closed broadcast channel tells live subscribers). A no-op when the orchestrator was built
    /// with [`new`](Self::new) alone rather than [`into_arc`](Self::into_arc) (the weak ref is empty),
    /// as in unit tests that never exercise capture death.
    fn spawn_capture_supervisor(
        &self,
        meeting_id: Uuid,
        died_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        let weak = self.self_weak.clone();
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
        let ane_gate = self.ane_gate.clone();
        let task_meeting = meeting.clone();
        let handle = tokio::spawn(async move {
            if let Some((refiner, threshold)) = refine {
                // Serialize the refine's ANE work (diarize + whisper) against any live meeting on the
                // shared permit: hold it only for the refine (waiting if a meeting currently holds
                // it), released before the transcript write (disk, not ANE). Deadlock-free — the live
                // side always releases at stop, this always releases when the refine returns.
                let _permit = ane_gate
                    .acquire_owned()
                    .await
                    .expect("ANE gate semaphore is never closed");
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
        // A pure read (P3): whether the pre-warmed pair for the next meeting has finished loading its
        // models. Warm *recovery* is handled off this path by the background warm ticker
        // (`spawn_warm_ticker`), so a UI status poll never spawns a sidecar process.
        self.backend.sidecars_ready()
    }

    /// Manual re-diarize: run the same refine + persist + transcript-rewrite path the auto-refine at
    /// stop uses, on demand for a stored meeting. Reuses the wired [`Refiner`], the effective
    /// recognition threshold, and [`queries::replace_them_segments`] — which is a no-op on an empty
    /// (silent / no-remote-speech) refine, so existing segments are never wiped. A genuine refine or
    /// persist failure surfaces as [`LiveError::Internal`]; a missing refiner / recording / audio is
    /// [`LiveError::Unavailable`]. The caller (the route) has already 404'd an unknown meeting id.
    async fn rediarize(&self, meeting_id: Uuid) -> Result<(), LiveError> {
        let refiner = self.refiner.clone().ok_or(LiveError::Unavailable)?;
        let meeting = queries::get_meeting(&self.pool, meeting_id)
            .await
            .map_err(OrchestratorError::from)?
            .ok_or(LiveError::Unavailable)?;
        let audio = meeting.dir_path(&self.output_dir).join("audio.wav");
        if !audio.exists() {
            return Err(LiveError::Unavailable);
        }
        // The effective recognition threshold (stored `speakers` override else the config default),
        // read fresh so a Settings change applies to the next re-diarize — matching the auto-refine.
        let (_auto_refine, threshold) = queries::effective_speakers(
            &self.pool,
            self.default_auto_refine,
            self.default_recognition_threshold,
        )
        .await
        .map_err(OrchestratorError::from)?;
        // Serialize against any live meeting on the shared ANE permit (waiting if one holds it), for
        // the refine's duration only — released before the DB write + transcript rewrite. The same
        // gate the auto-refine at stop takes; deadlock-free (the live side always releases at stop).
        let result = {
            let _permit = self
                .ane_gate
                .clone()
                .acquire_owned()
                .await
                .expect("ANE gate semaphore is never closed");
            refiner.refine(&audio).await?
        };
        queries::replace_them_segments(&self.pool, meeting_id, &result, threshold)
            .await
            .map_err(OrchestratorError::from)?;
        // Rewrite transcript.md + meeting.json from the refined (+ Me) segments (best-effort, logged).
        write_transcript(&self.pool, &self.output_dir, &meeting).await;
        Ok(())
    }

    async fn shutdown(&self) {
        self.wait_for_refines().await;
    }
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        // Stop the background warm ticker (a tracked, self-`Weak` task) promptly on shutdown rather
        // than waiting for its next tick to observe the dropped orchestrator.
        if let Some(handle) = self.warm_ticker.take() {
            handle.abort();
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

/// Resolve a per-meeting directory under `output_root` that does not already exist, so two same-title
/// meetings within one minute never resolve to (and a later delete then wipe) one shared folder.
/// Returns the chosen leaf `folder` name and its absolute path: `base` when free, else the first free
/// `base-2`, `base-3`, … suffix.
fn unique_meeting_dir(output_root: &Path, base: &str) -> (String, PathBuf) {
    let mut folder = base.to_string();
    let mut suffix = 1u32;
    loop {
        let dir = output_root.join(&folder);
        if !dir.exists() {
            return (folder, dir);
        }
        suffix += 1;
        folder = format!("{base}-{suffix}");
    }
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

    use std::collections::VecDeque;

    use hearsay_db::{connect_options, MIGRATOR};
    use sqlx::sqlite::SqlitePoolOptions;

    use crate::testing::{GateRefiner, ScriptedBackend, ScriptedSource, ScriptedTranscriber};
    use crate::traits::BackendInstance;
    use crate::types::{AudioChunk, CaptureChunk, Stream};

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

    #[test]
    fn unique_meeting_dir_suffixes_on_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let base = "2026-07-02_0905_weekly-sync";

        // A free base name is used as-is.
        let (folder, dir) = unique_meeting_dir(root, base);
        assert_eq!(folder, base);
        assert_eq!(dir, root.join(base));

        // Once that directory exists, the next resolves to `-2`, then `-3`.
        std::fs::create_dir(root.join(base)).unwrap();
        assert_eq!(unique_meeting_dir(root, base).0, format!("{base}-2"));
        std::fs::create_dir(root.join(format!("{base}-2"))).unwrap();
        assert_eq!(unique_meeting_dir(root, base).0, format!("{base}-3"));
    }

    async fn memory_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(connect_options("sqlite::memory:").unwrap())
            .await
            .unwrap();
        MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    fn them_chunk(host_ts: u64, samples: &[f32]) -> CaptureChunk {
        CaptureChunk {
            stream: Stream::Them,
            chunk: AudioChunk {
                host_ts,
                samples: samples.to_vec(),
            },
        }
    }

    /// One meeting's replay plan for [`RepeatingBackend`]: its Them-stream chunks plus the `Vec` its
    /// Them transcriber records its fed samples into.
    type MeetingPlan = (Vec<CaptureChunk>, Arc<Mutex<Vec<f32>>>);

    /// A [`Backend`] that hands out a fresh observable instance per meeting from a queue of plans
    /// (one per `build`), so a single orchestrator can run meeting A then meeting B. (`ScriptedBackend`
    /// can only build once, and `EmptyBackend` exposes no fed log.)
    struct RepeatingBackend {
        plans: Mutex<VecDeque<MeetingPlan>>,
    }

    impl Backend for RepeatingBackend {
        fn build(&self) -> BackendInstance {
            let (chunks, them_fed) = self
                .plans
                .lock()
                .unwrap()
                .pop_front()
                .expect("RepeatingBackend ran out of plans");
            BackendInstance {
                source: Box::new(ScriptedSource::new(chunks)),
                me: Box::new(ScriptedTranscriber::new(
                    vec![],
                    Arc::new(Mutex::new(Vec::new())),
                )),
                them: Box::new(ScriptedTranscriber::new(vec![], them_fed)),
            }
        }
    }

    /// P1: a live meeting holds the shared ANE permit for its whole duration and releases it at stop,
    /// so the offline refine (which takes the same permit) can never run on the ANE concurrently. The
    /// permit is taken off the start path (inside the pipeline's holder task), so poll for it.
    #[tokio::test]
    async fn live_meeting_holds_and_releases_the_ane_permit() {
        let pool = memory_pool().await;
        let tmp = tempfile::tempdir().unwrap();
        let (backend, _fed) = ScriptedBackend::new(vec![], vec![], vec![]);
        let orch = Orchestrator::new(pool, tmp.path().to_path_buf(), backend).into_arc();

        assert_eq!(
            orch.ane_gate.available_permits(),
            1,
            "idle: the ANE permit is free"
        );

        let meeting = orch.start_meeting(None).await.unwrap();
        let mut held = false;
        for _ in 0..10_000 {
            if orch.ane_gate.available_permits() == 0 {
                held = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(held, "a live meeting must hold the ANE permit");

        orch.stop_meeting(meeting.id).await.unwrap();
        // The permit drops when the aborted holder task is reaped, so poll for the release too.
        let mut released = false;
        for _ in 0..10_000 {
            if orch.ane_gate.available_permits() == 1 {
                released = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(released, "stop must release the ANE permit");
        orch.wait_for_refines().await;
    }

    /// P1: while a background refine holds the ANE permit, the *next* meeting's live sidecar feeding
    /// waits — it only begins once the refine releases the permit. Directly asserts refine and live
    /// never feed the ANE at the same time (recording is unaffected; only live feeding waits).
    #[tokio::test]
    async fn next_meeting_live_feeding_waits_for_the_refine_to_release_the_ane() {
        let pool = memory_pool().await;
        let tmp = tempfile::tempdir().unwrap();

        // record=false so neither meeting's recorder runs; A gets a manual audio.wav (so it
        // auto-refines at stop), B gets none (so B just finalizes — only its live phase is under test).
        queries::set_preference(&pool, queries::SECTION_RECORDING, r#"{"record":false}"#)
            .await
            .unwrap();

        let a_them_fed = Arc::new(Mutex::new(Vec::new()));
        let b_them_fed = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(RepeatingBackend {
            plans: Mutex::new(VecDeque::from(vec![
                (vec![], a_them_fed),
                (vec![them_chunk(0, &[1.0; 1600])], b_them_fed.clone()),
            ])),
        });

        let (refiner, gate) = GateRefiner::new();
        let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend)
            .with_refiner(refiner)
            .into_arc();

        let a = orch.start_meeting(Some("A".into())).await.unwrap();
        std::fs::write(tmp.path().join(&a.folder).join("audio.wav"), b"x").unwrap();
        orch.stop_meeting(a.id).await.unwrap();
        gate.started.notified().await; // refine A now holds the ANE permit

        // Meeting B starts promptly (the refine is off the op-lock); its live holder task blocks on
        // the held permit, so its Them sidecar must not be fed while the refine runs.
        let b = orch.start_meeting(Some("B".into())).await.unwrap();
        for _ in 0..2_000 {
            tokio::task::yield_now().await;
        }
        assert!(
            b_them_fed.lock().unwrap().is_empty(),
            "B must not feed the ANE while the refine holds the permit"
        );

        // Release the refine; it finishes and frees the ANE, so B's feeding can begin.
        gate.release.notify_one();
        let mut fed = false;
        for _ in 0..10_000 {
            if b_them_fed.lock().unwrap().len() == 1600 {
                fed = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(fed, "B must feed once the refine releases the ANE permit");

        orch.stop_meeting(b.id).await.unwrap();
        orch.wait_for_refines().await;
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1, "only A refined");
    }
}
