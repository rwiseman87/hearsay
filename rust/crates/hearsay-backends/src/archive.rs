//! Background archival of finalized meetings' audio.
//!
//! The recorder writes an uncompressed stereo `audio.wav` per meeting — 230 MB per hour — and
//! nothing ever shrinks it, so a regular user accumulates gigabytes of recordings with no way to
//! reclaim the space short of deleting meetings. This sweep re-encodes a finalized meeting's WAV as
//! lossless FLAC (~3x smaller) once it is older than the configured threshold. Being lossless it is
//! invisible downstream: playback, the offline refine, and re-diarization all read the archived file
//! and see identical samples.
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

/// Meetings whose archival has already failed this process, so a permanently unreadable recording is
/// not re-encoded on every tick. Deliberately in-process: the state resets on restart, which is the
/// right behavior after an upgrade fixes whatever broke, and it needs no schema change.
#[derive(Debug, Default)]
pub struct Sweeper {
    failed: Mutex<HashSet<Uuid>>,
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
    sweeper: &Sweeper,
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
    let cutoff = Utc::now() - chrono::Duration::days(after_days as i64);
    let candidates = match hearsay_db::queries::list_finalized_before(pool, cutoff).await {
        Ok(candidates) => candidates,
        Err(err) => {
            tracing::warn!(error = %err, "archive sweep: could not list meetings");
            return stats;
        }
    };

    for meeting in candidates {
        if busy() {
            tracing::debug!("archive sweep: a meeting started; stopping this pass");
            stats.deferred = true;
            break;
        }
        if sweeper.has_failed(meeting.id) {
            continue;
        }
        let dir = meeting.dir_path(output_dir);
        // Nothing to do when the meeting was never recorded, was already archived, or its folder is
        // gone. Checked before spawning so the common case costs one stat().
        if !dir.join(hearsay_audio::AUDIO_WAV).is_file() {
            continue;
        }

        let id = meeting.id;
        let job_dir = dir.clone();
        let result =
            tokio::task::spawn_blocking(move || hearsay_audio::compress_meeting_audio(&job_dir))
                .await;
        match result {
            Ok(Ok(out)) => {
                stats.compressed += 1;
                stats.reclaimed_bytes += out.wav_bytes.saturating_sub(out.flac_bytes);
                tracing::info!(
                    meeting = %id,
                    wav_bytes = out.wav_bytes,
                    flac_bytes = out.flac_bytes,
                    "archived meeting audio as flac"
                );
            }
            Ok(Err(err)) => {
                stats.failed += 1;
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
                sweeper.mark_failed(id);
                tracing::warn!(meeting = %id, error = %err, "archive task panicked");
            }
        }
    }
    stats
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
