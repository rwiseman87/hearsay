//! Background archival of finalized meetings' audio.
//!
//! Re-encodes a finalized meeting's stereo `audio.wav` as lossless FLAC once it is older than the
//! configured threshold. Being lossless it is invisible downstream. See `docs/architecture.md` for
//! the sweep's cadence and why it defers while a meeting is recording.
//!
//! Best-effort throughout, in the same spirit as [`crate::reconcile`]: a per-meeting failure is
//! logged and the sweep moves on, and [`hearsay_audio::compress_meeting_audio`] only removes a WAV
//! after proving the FLAC decodes back to it byte for byte. The sweep never touches a meeting that
//! is not `finalized`, and it yields entirely while a meeting is being recorded — encoding is CPU
//! work, and a live meeting owns the machine.
//!
//! A manual re-diarize does not change a meeting's status, so it can overlap a sweep of the same
//! meeting. That is safe rather than coordinated: the refine reads the whole track into memory
//! before the sweep could unlink anything, POSIX keeps an unlinked file readable through an open
//! handle, and on Windows a refused unlink leaves both files behind — which
//! [`hearsay_audio::resolve_recorded_audio`] resolves and the next sweep cleans up.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use hearsay_engine::LiveEngine;

/// How long to wait after a sweep before the next one. The threshold is in days, so the exact
/// cadence does not matter; hourly keeps a settings change taking effect promptly without polling
/// the disk in a tight loop.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Delay before the first sweep, so archival never competes with app launch.
const SWEEP_START_DELAY: Duration = Duration::from_secs(60);
/// How long to wait before re-checking after deferring to a live meeting. Deliberately far shorter
/// than [`SWEEP_INTERVAL`]: the app is usually opened in order to record, so the post-launch sweep
/// routinely lands inside a meeting. Waiting a full hour on that would mean a user who records and
/// quits never archives anything at all.
const SWEEP_BUSY_RETRY: Duration = Duration::from_secs(5 * 60);

/// What one sweep pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepStats {
    /// Meetings whose audio was archived.
    pub compressed: usize,
    /// Meetings whose archival failed (left as an uncompressed WAV).
    pub failed: usize,
    /// Bytes reclaimed.
    pub reclaimed_bytes: u64,
    /// The pass was skipped or cut short because a meeting was recording, so work may remain.
    pub deferred: bool,
}

/// Live progress of the current or most recent pass, for the Settings panel to poll.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepProgress {
    /// A pass is in flight.
    pub running: bool,
    /// Meetings this pass will process.
    pub total: usize,
    /// Meetings processed so far.
    pub done: usize,
    /// Meetings archived so far.
    pub compressed: usize,
    /// Meetings this pass could not archive.
    pub failed: usize,
    /// Bytes reclaimed so far.
    pub reclaimed_bytes: u64,
}

/// Shared archival state: which meetings have already failed this process, and how the current pass
/// is going.
///
/// The failure memo is deliberately in-process -- it resets on restart, which is the right behavior
/// after an upgrade fixes whatever broke, and it needs no schema change. The progress snapshot also
/// serves as the re-entrancy guard, so the Settings button and the periodic ticker cannot run two
/// passes over the same folders at once.
#[derive(Debug, Default)]
pub struct Sweeper {
    failed: Mutex<HashSet<Uuid>>,
    progress: Mutex<SweepProgress>,
}

impl Sweeper {
    pub fn new() -> Self {
        Self::default()
    }

    fn has_failed(&self, id: Uuid) -> bool {
        self.failed.lock().is_ok_and(|set| set.contains(&id))
    }

    fn mark_failed(&self, id: Uuid) {
        if let Ok(mut set) = self.failed.lock() {
            set.insert(id);
        }
    }

    /// The current pass snapshot (or the last one, once it has finished).
    pub fn progress(&self) -> SweepProgress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// True when a pass is in flight.
    pub fn is_running(&self) -> bool {
        self.progress().running
    }

    /// Claim the sweeper for a pass over `total` meetings, or `None` when one is already running.
    /// The returned guard clears `running` on drop, so an early return or a dropped task cannot
    /// wedge it.
    fn begin(self: &Arc<Self>, total: usize) -> Option<PassGuard> {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if progress.running {
            return None;
        }
        *progress = SweepProgress {
            running: true,
            total,
            ..SweepProgress::default()
        };
        drop(progress);
        Some(PassGuard {
            sweeper: Arc::clone(self),
        })
    }

    /// Record one processed meeting.
    fn step(&self, compressed: bool, reclaimed: u64) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.done += 1;
        if compressed {
            progress.compressed += 1;
            progress.reclaimed_bytes += reclaimed;
        } else {
            progress.failed += 1;
        }
    }
}

/// Clears the running flag when a pass ends, however it ends. Owns its `Arc` so a pass handed to a
/// background task keeps the reservation for as long as the task lives.
struct PassGuard {
    sweeper: Arc<Sweeper>,
}

impl Drop for PassGuard {
    fn drop(&mut self) {
        let mut progress = self
            .sweeper
            .progress
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        progress.running = false;
    }
}

/// The meetings a pass would actually touch: aged out, not already failed this process, and still
/// holding an uncompressed recording. Resolved up front so the reported total is real work rather
/// than every aged row -- one `stat()` each. Nothing to do when a meeting was never recorded, was
/// already archived, or its folder is gone.
pub async fn plan(
    pool: &SqlitePool,
    output_dir: &Path,
    after_days: u64,
    sweeper: &Sweeper,
) -> Vec<hearsay_db::models::Meeting> {
    let cutoff = Utc::now() - chrono::Duration::days(after_days as i64);
    let candidates = match hearsay_db::queries::list_finalized_before(pool, cutoff).await {
        Ok(candidates) => candidates,
        Err(err) => {
            tracing::warn!(error = %err, "archive sweep: could not list meetings");
            return Vec::new();
        }
    };
    candidates
        .into_iter()
        .filter(|m| !sweeper.has_failed(m.id))
        .filter(|m| {
            m.dir_path(output_dir)
                .join(hearsay_audio::AUDIO_WAV)
                .is_file()
        })
        .collect()
}

/// Run one archival pass over every finalized meeting older than `after_days`.
///
/// `busy` short-circuits the whole sweep — the caller passes whether a meeting is currently active.
/// It is re-checked between meetings too, so a meeting starting mid-sweep stops the remaining work
/// rather than contending with the live pipeline for the rest of the pass.
pub async fn sweep_once(
    pool: &SqlitePool,
    output_dir: &Path,
    enabled: bool,
    after_days: u64,
    sweeper: &Arc<Sweeper>,
    busy: &(dyn Fn() -> bool + Send + Sync),
) -> SweepStats {
    let mut stats = SweepStats::default();
    if !enabled {
        return stats;
    }
    if busy() {
        stats.deferred = true;
        return stats;
    }
    let work = plan(pool, output_dir, after_days, sweeper).await;
    if work.is_empty() {
        return stats;
    }
    // Also the re-entrancy guard: the Settings button and the ticker must not sweep at once.
    let Some(_pass) = sweeper.begin(work.len()) else {
        tracing::debug!("archive sweep: a pass is already running");
        stats.deferred = true;
        return stats;
    };
    run_pass(work, output_dir, sweeper, busy).await
}

/// Archive each meeting in `work`, stopping early if a meeting starts recording. The caller owns the
/// pass reservation, so this is shared by the periodic sweep and the on-demand one.
async fn run_pass(
    work: Vec<hearsay_db::models::Meeting>,
    output_dir: &Path,
    sweeper: &Arc<Sweeper>,
    busy: &(dyn Fn() -> bool + Send + Sync),
) -> SweepStats {
    let mut stats = SweepStats::default();
    for meeting in work {
        if busy() {
            tracing::debug!("archive sweep: a meeting started; stopping this pass");
            stats.deferred = true;
            break;
        }
        let dir = meeting.dir_path(output_dir);
        let id = meeting.id;
        let job_dir = dir.clone();
        let result =
            tokio::task::spawn_blocking(move || hearsay_audio::compress_meeting_audio(&job_dir))
                .await;
        match result {
            Ok(Ok(out)) => {
                let reclaimed = out.wav_bytes.saturating_sub(out.flac_bytes);
                stats.compressed += 1;
                stats.reclaimed_bytes += reclaimed;
                sweeper.step(true, reclaimed);
                tracing::info!(
                    meeting = %id,
                    wav_bytes = out.wav_bytes,
                    flac_bytes = out.flac_bytes,
                    "archived meeting audio as flac"
                );
            }
            Ok(Err(err)) => {
                stats.failed += 1;
                sweeper.step(false, 0);
                // Only remember failures that will fail again. A bad or unreadable recording is
                // deterministic, so retrying it hourly forever is pure waste. An I/O failure is not:
                // a manual re-diarize on an old meeting holds the wav open (Windows refuses to
                // unlink it), and that resolves on its own — memoizing it would strand the meeting
                // uncompressed until the next restart.
                if !matches!(err, hearsay_audio::AudioError::Io(_)) {
                    sweeper.mark_failed(id);
                }
                tracing::warn!(
                    meeting = %id,
                    error = %err,
                    "failed to archive meeting audio; keeping the wav"
                );
            }
            Err(err) => {
                stats.failed += 1;
                sweeper.step(false, 0);
                sweeper.mark_failed(id);
                tracing::warn!(meeting = %id, error = %err, "archive task panicked");
            }
        }
    }
    stats
}

/// Start a pass in the background and return its opening snapshot, with the work list already
/// counted. `None` when a pass is already running.
///
/// The plan is resolved before returning so the caller's response reports a real `total` and a
/// `running` flag that is already true -- a UI that polls on `running` would otherwise miss the
/// start of its own request. The pass itself is spawned, because a backlog takes far longer than a
/// request should hold.
pub async fn start_background_pass(
    pool: SqlitePool,
    output_dir: PathBuf,
    after_days: u64,
    sweeper: Arc<Sweeper>,
    engine: Arc<dyn LiveEngine>,
) -> Option<SweepProgress> {
    let work = plan(&pool, &output_dir, after_days, &sweeper).await;
    let pass = sweeper.begin(work.len())?;
    let snapshot = sweeper.progress();
    tokio::spawn(async move {
        let stats = run_pass(work, &output_dir, &sweeper, &|| {
            engine.active_meeting().is_some()
        })
        .await;
        drop(pass);
        tracing::info!(
            compressed = stats.compressed,
            failed = stats.failed,
            reclaimed_bytes = stats.reclaimed_bytes,
            "archive pass finished (requested from settings)"
        );
    });
    Some(snapshot)
}

/// Spawn the periodic archival sweep.
///
/// Holds a [`Weak`] engine ref so the task stops once the engine is dropped (mirroring
/// `Orchestrator::spawn_warm_ticker`), and re-reads the effective settings each tick so a Settings
/// change applies with no restart. The first tick fires after [`SWEEP_START_DELAY`], which doubles
/// as the startup sweep.
pub fn spawn_archive_ticker(
    pool: SqlitePool,
    output_dir: PathBuf,
    engine: Weak<dyn LiveEngine>,
    sweeper: Arc<Sweeper>,
    default_enabled: bool,
    default_days: u64,
) {
    tokio::spawn(async move {
        // A fixed-period interval is wrong here: deferring to a live meeting must cost
        // minutes, not the whole period, so the next delay is chosen per iteration.
        let mut delay = SWEEP_START_DELAY;
        loop {
            tokio::time::sleep(delay).await;
            let Some(engine) = engine.upgrade() else {
                return;
            };
            if engine.active_meeting().is_some() {
                tracing::debug!("archive sweep: deferring, a meeting is recording");
                delay = SWEEP_BUSY_RETRY;
                continue;
            }
            delay = SWEEP_INTERVAL;
            let (enabled, days) = match hearsay_db::queries::effective_compression(
                &pool,
                default_enabled,
                default_days,
            )
            .await
            {
                Ok(values) => values,
                Err(err) => {
                    tracing::warn!(error = %err, "archive sweep: could not read settings");
                    continue;
                }
            };
            let busy = || engine.active_meeting().is_some();
            let stats = sweep_once(&pool, &output_dir, enabled, days, &sweeper, &busy).await;
            if stats.compressed > 0 || stats.failed > 0 {
                tracing::info!(
                    compressed = stats.compressed,
                    failed = stats.failed,
                    reclaimed_bytes = stats.reclaimed_bytes,
                    "archive sweep finished"
                );
            }
            // A meeting that started mid-pass cuts the pass short, so come back in minutes with the
            // rest of the backlog rather than in an hour.
            if stats.deferred {
                delay = SWEEP_BUSY_RETRY;
            }
        }
    });
}
