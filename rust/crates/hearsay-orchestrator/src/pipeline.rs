//! The live transcription pipeline: route each stream's PCM to its transcriber and persist +
//! broadcast the segments it emits.
//!
//! Task layout:
//! - `demux` — reads the capture channel, anchors the shared epoch clock, and forwards each chunk
//!   to its stream's task as meeting-relative `(t0_s, samples)`.
//! - one task per stream — feeds its transcriber and handles the segments it emits: partials
//!   broadcast to the UI only; finals also persist to the database (Them binds a `Speaker N`
//!   cluster). Segment times are shifted by the stream's offset (its first fed `t0_s`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

use hearsay_db::queries;

use crate::aec::EchoCanceller;
use crate::echo_dedup::EchoDedup;
use crate::error::OrchestratorError;
use crate::lock::MutexExt;
use crate::recorder::MeetingAudioRecorder;
use crate::traits::{AudioSource, BackendInstance, StreamRole, Transcriber};
use crate::tuning::{LiveStats, LiveTuning};
use crate::types::{CaptureChunk, SegmentKind, SidecarSegment, Stream};

/// Capacity of the per-meeting live broadcast channel (transcript events to WebSocket subscribers).
const BROADCAST_CAPACITY: usize = 256;

/// Nominal capture chunk length (the helper drains every 20 ms); sizes the hand-off channels.
const NOMINAL_CHUNK_MS: usize = 20;

/// Seconds of audio each stream's demux -> stream task channel buffers. On overflow demux drops
/// that stream's chunks (counted) rather than stall the recorder or the other stream.
const PCM_BUFFER_SECONDS: usize = 10;

/// Capacity of each stream's PCM hand-off channel: [`PCM_BUFFER_SECONDS`] of nominal chunks (500
/// slots, under 1 MB of PCM per stream).
const PCM_CHANNEL_CAPACITY: usize = PCM_BUFFER_SECONDS * 1000 / NOMINAL_CHUNK_MS;

/// Minimum spacing between drop warnings for one stream; drops in between accumulate into the next.
const DROP_WARN_INTERVAL: Duration = Duration::from_secs(5);

/// Seconds of audio the demux -> recorder channel buffers to absorb a disk stall. On overflow demux
/// drops the chunk from the archive (a gap the recorder re-anchors over).
const REC_BUFFER_SECONDS: usize = 50;

/// Capacity of the demux -> recorder channel: [`REC_BUFFER_SECONDS`] of nominal chunks (2500 slots,
/// a few MB of PCM).
const REC_CHANNEL_CAPACITY: usize = REC_BUFFER_SECONDS * 1000 / NOMINAL_CHUNK_MS;

/// A stream whose sidecar has been running at least this long when it dies gets a fresh retry
/// budget, so isolated crashes over a long meeting do not exhaust it.
const RESPAWN_STABLE: Duration = Duration::from_secs(60);

const SAMPLE_RATE: f64 = hearsay_audio::SAMPLE_RATE as f64;

/// One raw capture chunk handed to the recorder task: `(samples, t0_s, stream)`.
type RecordChunk = (Vec<f32>, f64, Stream);

/// Re-anchor a sidecar's sample-count timeline to the chunk's `t0_s` only once they diverge past
/// this — a real delivery gap (dropped frames, a tap rebuild, a wedged-then-recovered stream), not
/// per-chunk clock jitter. Matches the recorder's `RESYNC_GAP` (0.2 s) so the sidecar timeline and
/// `audio.wav` re-anchor together and transcript times stay aligned.
const RESYNC_THRESHOLD_S: f64 = 0.2;

/// Safety cap on one silence-pad fed to a sidecar during a resync, so a bad (non-monotonic) `host_ts`
/// jump cannot force a multi-GB allocation. 5 min of 16 kHz mono — far beyond any real gap. Past
/// this the timeline diverges by the excess (logged); acceptable, as the recorder re-anchors on
/// `t0_s` too.
const MAX_SILENCE_PAD_SAMPLES: usize = 5 * 60 * hearsay_audio::SAMPLE_RATE as usize;

/// Continuous all-zero mic samples before [`DeadMicMonitor`] reports the signal path dead. 10 s at
/// 16 kHz: long enough that a legitimately digital-silent stretch (a resync pad, a codec dropout)
/// never trips it, short enough to catch a muted mic early in a meeting rather than at the end.
const DEAD_MIC_AFTER_SAMPLES: u64 = 10 * hearsay_audio::SAMPLE_RATE as u64;

/// How often each stream broadcasts its recent RMS amplitude as a `level` frame (drives the live
/// input waveform). ~10 Hz: smooth enough for a VU meter, sparse enough that it never crowds the
/// bounded broadcast buffer the transcript shares.
const LEVEL_INTERVAL: Duration = Duration::from_millis(100);

/// How often the inactivity watchdog re-checks the silence clock. Coarse (the thresholds are
/// minutes), so the tick cost is negligible; fine enough that a prompt/auto-end fires within a few
/// seconds of crossing its threshold.
const INACTIVITY_TICK: Duration = Duration::from_secs(15);

/// Resolved inactivity-watchdog thresholds for one meeting (the effective `recording` settings). The
/// prompt and the auto-end are independently toggleable: the watchdog nudges the UI after
/// `prompt_after` of silence when `prompt_enabled`, and auto-ends the meeting after `end_after` when
/// `auto_end_enabled`. With both off, no watchdog is spawned.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InactivityConfig {
    /// Whether to nudge the UI with a "still recording?" prompt after `prompt_after` of silence.
    pub prompt_enabled: bool,
    /// Whether to auto-end the meeting (with a logged transcript marker) after `end_after` of silence.
    pub auto_end_enabled: bool,
    /// Continuous silence before the in-app "still recording?" prompt.
    pub prompt_after: Duration,
    /// Continuous silence before the meeting auto-ends.
    pub end_after: Duration,
}

impl InactivityConfig {
    /// Whether a watchdog is needed at all (either escalation is enabled).
    fn active(&self) -> bool {
        self.prompt_enabled || self.auto_end_enabled
    }
}

/// How long [`Pipeline::close`] waits for a stream task to wind down before aborting it. Above the
/// transcriber's own close deadline (drain + reap, 10 s) so a *healthy* sidecar's graceful tail
/// flush always completes; a *wedged* sidecar (its stream task blocked feeding a full pipe) is
/// aborted here so meeting stop cannot hang. Aborting drops the `ProcessTranscriber`, whose
/// `kill_on_drop` reaps the child.
const STREAM_JOIN_TIMEOUT: Duration = Duration::from_secs(15);

/// A running pipeline: the capture source (kept to stop it) and the spawned tasks.
pub(crate) struct Pipeline {
    pub(crate) broadcast_tx: broadcast::Sender<String>,
    pub(crate) source: Box<dyn AudioSource>,
    /// Set true by [`close`](Self::close) before stopping the source, so demux can tell an
    /// intentional stop from an unexpected capture death (helper crash / socket EOF).
    intentional_stop: Arc<AtomicBool>,
    /// Reads capture, echo-cancels Me, hands raw chunks to the recorder task, and fans PCM to the
    /// stream tasks. Cannot block — it drops-with-log on a full stream/recorder queue — so it finishes
    /// promptly once capture ends.
    demux: JoinHandle<()>,
    /// The `audio.wav` writer task (a dedicated blocking thread fed by demux), or `None` when
    /// recording is disabled. Awaited unbounded on close so the final WAV encode + finalize completes
    /// before stop returns (the refine that follows reads the finished file).
    recorder: Option<JoinHandle<()>>,
    /// The per-stream feed+persist tasks. Bounded on close (a wedged sidecar can block one in
    /// `feed`), then aborted.
    streams: Vec<JoinHandle<()>>,
    /// Holds the shared single ANE permit for this meeting's lifetime — acquired off the start path
    /// (in a dedicated task) so neither `start_meeting` nor recording ever blocks on it. While it is
    /// held, the offline refine (which takes the same permit) cannot run on the ANE. Aborted by
    /// [`close`](Self::close) to release the permit at stop/teardown (P1).
    ane_holder: JoinHandle<()>,
    /// Watchers that each await one sidecar's models-ready signal, then (whichever is last) broadcast
    /// a `ready` status. Tracked so [`close`](Self::close) can abort them at teardown: a sidecar that
    /// never signals ready would otherwise leave its watcher parked forever holding a `broadcast_tx`
    /// clone, keeping the broadcast channel open past the pipeline's life.
    ready_watchers: Vec<JoinHandle<()>>,
    /// True while any transcription sidecar is still loading its models (a cold start); false once
    /// all are serving. Read by the orchestrator to answer the WebSocket warm-up snapshot so the UI
    /// can show a "preparing" notice instead of a silent gap.
    pub(crate) warming: Arc<AtomicBool>,
    /// The inactivity watchdog task (silence prompt + auto-end). `None` when the feature is disabled.
    /// Aborted by [`close`](Self::close): a parked watchdog holding a `broadcast_tx` clone would keep
    /// the broadcast channel open past the pipeline's life (same reason as `ready_watchers`).
    inactivity_watchdog: Option<JoinHandle<()>>,
    /// The silence clock: the [`Instant`] of the last emitted segment on either stream. Set by the
    /// stream tasks on every segment and reset by [`keep_alive`](Self::keep_alive) (the "Keep
    /// recording" action). The watchdog measures silence as its elapsed time.
    last_activity: Arc<Mutex<Instant>>,
    /// The current inactivity-prompt state for the WebSocket connect-snapshot: `Some(silent_seconds)`
    /// while a prompt is active, `None` otherwise. Updated by the watchdog.
    pub(crate) inactivity_prompt: watch::Receiver<Option<u64>>,
    /// The pause gate (the "Pause" control). While set, demux drops chunks and elides the paused span
    /// from the timeline (so `audio.wav` + segment times stay contiguous), and the watchdog holds the
    /// silence clock. Read for the WebSocket connect-snapshot.
    pub(crate) paused: Arc<AtomicBool>,
}

impl Pipeline {
    /// Reset the silence clock to now — the "Keep recording" action. The next watchdog tick then sees
    /// no silence, clears any active prompt, and re-arms; the auto-end is measured afresh from here.
    pub(crate) fn keep_alive(&self) {
        *self.last_activity.lock_recover() = Instant::now();
    }

    /// Pause capture: demux stops recording/forwarding and the timeline freezes. Idempotent.
    /// Broadcasts a `capture_state` frame so live subscribers freeze the timer/waveform.
    pub(crate) fn pause(&self) {
        if !self.paused.swap(true, Ordering::SeqCst) {
            publish_capture_state(&self.broadcast_tx, "paused");
        }
    }

    /// Resume capture after a pause. Resets the silence clock so the just-elapsed paused span is not
    /// counted as inactivity, and broadcasts a `capture_state` frame. Idempotent.
    pub(crate) fn resume(&self) {
        if self.paused.swap(false, Ordering::SeqCst) {
            *self.last_activity.lock_recover() = Instant::now();
            publish_capture_state(&self.broadcast_tx, "active");
        }
    }

    /// Whether capture is currently paused (for the WebSocket connect-snapshot).
    pub(crate) fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Stop capture, then wait for the tasks to wind down (each transcriber's tail is drained on
    /// close). After this returns, the broadcast channel closes when the pipeline is dropped.
    pub(crate) async fn close(mut self) {
        // Mark this an intentional stop before closing capture, so demux does not report the
        // resulting capture-end as an unexpected death.
        self.intentional_stop.store(true, Ordering::SeqCst);
        // Release the shared ANE permit now: aborting the holder drops the permit (freeing the ANE
        // for the next refine) and, by dropping its readiness sender, unblocks a stream loop still
        // waiting on it (e.g. if a prior refine held the ANE for this whole meeting), so stop never
        // waits the full join timeout for one.
        self.ane_holder.abort();
        // Abort the ready-watchers: a sidecar that never signals ready would otherwise leave its
        // watcher parked forever holding a `broadcast_tx` clone, keeping the broadcast channel open
        // past the pipeline's life.
        for watcher in self.ready_watchers.drain(..) {
            watcher.abort();
        }
        // Abort the inactivity watchdog for the same reason (it holds a `broadcast_tx` clone). On an
        // intentional stop it is simply no longer needed; on the auto-end path it has already fired
        // and exited, so this is a no-op there.
        if let Some(watchdog) = self.inactivity_watchdog.take() {
            watchdog.abort();
        }
        self.source.stop().await;
        // Demux never blocks (it drops-with-log on a full stream/recorder queue), so it finishes
        // promptly after capture closes. Await it, which drops its recorder sender.
        let _ = self.demux.await;
        // The recorder task then drains its queue, writes the tail, and finalizes `audio.wav`; await
        // it unbounded so the encode completes before stop returns (the refine reads the file).
        if let Some(recorder) = self.recorder.take() {
            let _ = recorder.await;
        }
        // A wedged sidecar can leave its stream task blocked in `feed`; bound the join and abort so
        // stop cannot hang. Abort drops the transcriber, whose `kill_on_drop` reaps the child.
        for task in self.streams.drain(..) {
            let abort = task.abort_handle();
            if tokio::time::timeout(STREAM_JOIN_TIMEOUT, task)
                .await
                .is_err()
            {
                tracing::warn!("stream task did not wind down within the deadline; aborting");
                abort.abort();
            }
        }
    }
}

/// Start the source + both transcribers and spawn the routing/handling tasks. `audio_path` is the
/// `audio.wav` to record (Me=L / Them=R) when recording is enabled, else `None`. Returns the
/// pipeline plus two one-shot receivers the orchestrator finalizes the meeting on: the first fires if
/// capture ends **unexpectedly** (helper crash / socket EOF) rather than via [`Pipeline::close`], the
/// second fires when the inactivity watchdog auto-ends the meeting after sustained silence. Either
/// path routes through `stop_meeting` so the meeting is never left falsely live.
pub(crate) async fn spawn(
    instance: BackendInstance,
    pool: SqlitePool,
    meeting_id: Uuid,
    audio_path: Option<PathBuf>,
    ane_gate: Arc<Semaphore>,
    inactivity: InactivityConfig,
    tuning: &LiveTuning,
) -> Result<(Pipeline, oneshot::Receiver<()>, oneshot::Receiver<()>), OrchestratorError> {
    let BackendInstance {
        mut source,
        mut me,
        mut them,
    } = instance;

    // Start the transcribers first, then the capture source. A cold sidecar's start() forks the
    // process and returns immediately, so its ~10 s CoreML/ANE model load runs *during* the capture
    // handshake below (Core Audio graph build + any first-run TCC prompt) rather than stacking after
    // it — cutting time-to-first-transcript. (A pre-warmed sidecar's start() returns instantly.) If
    // a later stage fails, tear down what already started (a started transcriber holds a live
    // sidecar; the source keeps the mic/tap hot) instead of dropping it un-stopped.
    let me_emit = me.start().await?;
    let them_emit = match them.start().await {
        Ok(rx) => rx,
        Err(err) => {
            me.close().await;
            return Err(err);
        }
    };
    let capture_rx = match source.start().await {
        Ok(rx) => rx,
        Err(err) => {
            them.close().await;
            me.close().await;
            return Err(err);
        }
    };

    let (broadcast_tx, _) = broadcast::channel::<String>(BROADCAST_CAPACITY);

    // Transcription warm-up state for the UI. A cold sidecar exposes a ready one-shot (still loading
    // its models); a pre-warmed one does not (already serving). While any sidecar is still loading,
    // `warming` is true — the WS sends a "preparing" snapshot to new subscribers. Once the last one
    // is ready, flip it false and broadcast a `ready` status so an already-connected client clears
    // the notice.
    let ready_signals: Vec<oneshot::Receiver<()>> = [me.ready_signal(), them.ready_signal()]
        .into_iter()
        .flatten()
        .collect();
    let warming = Arc::new(AtomicBool::new(!ready_signals.is_empty()));
    let mut ready_watchers = Vec::new();
    if !ready_signals.is_empty() {
        let pending = Arc::new(AtomicUsize::new(ready_signals.len()));
        for ready_rx in ready_signals {
            let warming = warming.clone();
            let pending = pending.clone();
            let broadcast_tx = broadcast_tx.clone();
            ready_watchers.push(tokio::spawn(async move {
                // Fires on the sidecar's ready marker; Err if it died first — either way it is no
                // longer loading, so count it down.
                let _ = ready_rx.await;
                if pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                    warming.store(false, Ordering::SeqCst);
                    publish_status(&broadcast_tx, "ready");
                }
            }));
        }
    }

    // Serialize this meeting's live ANE work against the offline refine (which takes the same
    // permit): hold the shared single ANE permit for the meeting's lifetime. Acquired in a dedicated
    // task so neither `start_meeting`'s caller nor recording ever blocks on it — only live feeding
    // waits, and only when a prior meeting's refine is still finishing. `ane_ready` flips true once
    // the permit is held; the stream loops gate their first feed on it. The holder parks holding the
    // permit until `Pipeline::close` aborts it (dropping the permit and this readiness sender).
    let (ane_ready_tx, ane_ready_rx) = watch::channel(false);
    let ane_holder = tokio::spawn(async move {
        // `acquire_owned` errors only if the semaphore is closed, which never happens; on that (never)
        // path the readiness stays false and the stream loops wind down at close.
        if let Ok(_permit) = ane_gate.acquire_owned().await {
            let _ = ane_ready_tx.send(true);
            std::future::pending::<()>().await;
        }
    });

    let (me_tx, me_rx) = mpsc::channel::<(f64, Vec<f32>)>(PCM_CHANNEL_CAPACITY);
    let (them_tx, them_rx) = mpsc::channel::<(f64, Vec<f32>)>(PCM_CHANNEL_CAPACITY);

    // The silence clock the inactivity watchdog measures. Seeded to now so the warm-up gap (a cold
    // start emits no segments for ~10 s) counts as silence from meeting start, never as a spurious
    // long-idle head start. Each stream task stamps it on every emitted segment.
    let meeting_start = Instant::now();
    let last_activity = Arc::new(Mutex::new(meeting_start));
    // The current prompt state for the WS connect-snapshot; the watchdog drives it.
    let (prompt_tx, prompt_rx) = watch::channel(None::<u64>);
    // Fires once when the watchdog auto-ends the meeting after sustained silence; the orchestrator
    // finalizes on it (mirroring `died_rx`). When the feature is disabled, the sender is dropped so
    // the receiver resolves `Err` and the orchestrator's supervisor no-ops.
    let (inactive_tx, inactive_rx) = oneshot::channel();

    // Record audio.wav on its own blocking thread fed by a dedicated queue, so no disk write ever
    // runs on the never-block demux path. `None` when recording is disabled.
    let (rec_tx, recorder) = match audio_path {
        Some(path) => {
            let (tx, rx) = mpsc::channel::<RecordChunk>(REC_CHANNEL_CAPACITY);
            let task = tokio::task::spawn_blocking(move || {
                recorder_loop(rx, MeetingAudioRecorder::new(path))
            });
            (Some(tx), Some(task))
        }
        None => (None, None),
    };
    // Shared across both stream tasks: the Them task records finals as candidate echo sources; the
    // Me task drops a final that matches one. A backstop behind the acoustic canceller (`aec`), it
    // touches only live Me finals, never the archive or the refine.
    let dedup = tuning
        .echo_dedup
        .map_or_else(EchoDedup::off, EchoDedup::new);
    let echo_dedup = Arc::new(Mutex::new(dedup.with_stats(tuning.stats.clone())));
    let intentional_stop = Arc::new(AtomicBool::new(false));
    // Pause gate (the "Pause" control): while set, demux drops chunks and elides the span so the
    // timeline stays contiguous, and the watchdog holds the silence clock. Shared with those tasks.
    let paused = Arc::new(AtomicBool::new(false));
    let (died_tx, died_rx) = oneshot::channel();
    let demux = tokio::spawn(demux(
        capture_rx,
        me_tx,
        them_tx,
        rec_tx,
        EchoCanceller::new_with(tuning.aec),
        intentional_stop.clone(),
        paused.clone(),
        died_tx,
        tuning.stats.clone(),
    ));
    let me_task = tokio::spawn(stream_loop(
        StreamRole::Me,
        me,
        me_rx,
        me_emit,
        pool.clone(),
        meeting_id,
        broadcast_tx.clone(),
        ane_ready_rx.clone(),
        last_activity.clone(),
        echo_dedup.clone(),
        tuning.stats.clone(),
    ));
    let them_task = tokio::spawn(stream_loop(
        StreamRole::Them,
        them,
        them_rx,
        them_emit,
        pool.clone(),
        meeting_id,
        broadcast_tx.clone(),
        ane_ready_rx,
        last_activity.clone(),
        echo_dedup,
        tuning.stats.clone(),
    ));

    // Spawn the inactivity watchdog only when a prompt or an auto-end is enabled; otherwise drop
    // `inactive_tx` (no auto-end) and leave `prompt_rx` parked at `None`.
    let inactivity_watchdog = if inactivity.active() {
        Some(tokio::spawn(inactivity_watchdog(
            inactivity,
            INACTIVITY_TICK,
            last_activity.clone(),
            meeting_start,
            prompt_tx,
            broadcast_tx.clone(),
            pool,
            meeting_id,
            inactive_tx,
            paused.clone(),
        )))
    } else {
        drop(inactive_tx);
        None
    };

    Ok((
        Pipeline {
            broadcast_tx,
            source,
            intentional_stop,
            demux,
            recorder,
            streams: vec![me_task, them_task],
            ane_holder,
            ready_watchers,
            warming,
            inactivity_watchdog,
            last_activity,
            inactivity_prompt: prompt_rx,
            paused,
        },
        died_rx,
        inactive_rx,
    ))
}

/// Chunks and samples dropped on a full hand-off channel, with the rate limiter for the drop warning.
#[derive(Default)]
struct DropStats {
    chunks: u64,
    samples: u64,
    pending_chunks: u64,
    pending_samples: u64,
    last_warn: Option<Instant>,
}

impl DropStats {
    /// Count one dropped chunk. Returns the `(chunks, samples)` dropped since the last warning once
    /// [`DROP_WARN_INTERVAL`] has elapsed (or on the first drop), else `None`.
    fn record(&mut self, samples: usize, now: Instant) -> Option<(u64, u64)> {
        self.chunks += 1;
        self.samples += samples as u64;
        self.pending_chunks += 1;
        self.pending_samples += samples as u64;
        if self
            .last_warn
            .is_some_and(|t| now.duration_since(t) < DROP_WARN_INTERVAL)
        {
            return None;
        }
        self.last_warn = Some(now);
        Some((
            std::mem::take(&mut self.pending_chunks),
            std::mem::take(&mut self.pending_samples),
        ))
    }
}

/// Hands one stream's chunks to its transcriber task without ever blocking on a slow/wedged one (that
/// would stall the recorder + the other stream); on a full queue it drops and counts. The dropped
/// span reappears as a timeline gap that `stream_loop`'s resync pads with silence, so segment times
/// stay aligned.
struct PcmForwarder {
    stream: Stream,
    sender: mpsc::Sender<(f64, Vec<f32>)>,
    stats: DropStats,
    live_stats: Option<Arc<LiveStats>>,
}

impl PcmForwarder {
    fn new(stream: Stream, sender: mpsc::Sender<(f64, Vec<f32>)>) -> Self {
        PcmForwarder {
            stream,
            sender,
            stats: DropStats::default(),
            live_stats: None,
        }
    }

    fn with_stats(mut self, stats: Option<Arc<LiveStats>>) -> Self {
        self.live_stats = stats;
        self
    }

    fn forward(&mut self, t0_s: f64, samples: Vec<f32>) {
        let len = samples.len();
        match self.sender.try_send((t0_s, samples)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                if let Some(live) = &self.live_stats {
                    live.dropped_chunks.fetch_add(1, Ordering::SeqCst);
                }
                if let Some((chunks, samples)) = self.stats.record(len, Instant::now()) {
                    tracing::warn!(
                        stream = ?self.stream,
                        chunks,
                        seconds = samples as f64 / SAMPLE_RATE,
                        "transcriber queue full; dropping audio",
                    );
                }
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }

    fn log_summary(&self) {
        let seconds = self.stats.samples as f64 / SAMPLE_RATE;
        if self.stats.chunks > 0 {
            tracing::warn!(
                stream = ?self.stream,
                dropped_chunks = self.stats.chunks,
                dropped_samples = self.stats.samples,
                dropped_seconds = seconds,
                "audio dropped on a full transcriber queue this meeting",
            );
        } else {
            tracing::info!(stream = ?self.stream, "no audio dropped on the transcriber queue");
        }
    }
}

/// The recorder task body: drain raw chunks from demux and write them to `audio.wav`, then finalize
/// on channel close (demux's sender dropped). Runs on a dedicated blocking thread (`spawn_blocking`),
/// so its buffered disk writes never touch the async runtime or the never-block demux path.
/// Best-effort — a write/finalize failure is logged, never fails the meeting stop.
fn recorder_loop(mut rx: mpsc::Receiver<RecordChunk>, mut recorder: MeetingAudioRecorder) {
    while let Some((samples, t0_s, stream)) = rx.blocking_recv() {
        recorder.write(&samples, t0_s, stream);
    }
    if let Err(err) = recorder.close() {
        tracing::error!(error = %err, "failed to finalize meeting audio.wav");
    }
}

/// Read capture, anchor the shared epoch on the first chunk, hand the stereo `audio.wav` chunks to the
/// recorder task (if enabled), and forward each chunk to its stream's task as meeting-relative
/// `(t0_s, samples)`. Both streams anchor to the same epoch so their timelines align (alignment is by
/// timestamp, never sample index). Me is echo-cancelled against the Them tap before it reaches
/// transcription; the recording stays raw. The recorder task finalizes once this returns (its sender
/// drops).
#[allow(clippy::too_many_arguments)]
async fn demux(
    mut capture_rx: mpsc::Receiver<CaptureChunk>,
    me_tx: mpsc::Sender<(f64, Vec<f32>)>,
    them_tx: mpsc::Sender<(f64, Vec<f32>)>,
    rec_tx: Option<mpsc::Sender<RecordChunk>>,
    mut canceller: EchoCanceller,
    intentional_stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    died_tx: oneshot::Sender<()>,
    stats: Option<Arc<LiveStats>>,
) {
    let mut me_fwd = PcmForwarder::new(Stream::Me, me_tx).with_stats(stats.clone());
    let mut them_fwd = PcmForwarder::new(Stream::Them, them_tx).with_stats(stats);
    let mut epoch_ns: Option<u64> = None;
    // Total nanoseconds elided by pauses, subtracted from every chunk's meeting time so the timeline
    // (and `audio.wav`) stays contiguous across a pause — no silent gap. `pause_started_at` holds the
    // `host_ts` of the first chunk dropped in the current pause, so the whole span can be subtracted
    // on resume.
    let mut paused_ns: u64 = 0;
    let mut pause_started_at: Option<u64> = None;
    while let Some(cap) = capture_rx.recv().await {
        if paused.load(Ordering::SeqCst) {
            // Freeze the timeline: drop the chunk (neither recorded nor transcribed) and remember
            // when the pause began so its full span can be elided when capture resumes.
            pause_started_at.get_or_insert(cap.chunk.host_ts);
            continue;
        }
        if let Some(started) = pause_started_at.take() {
            paused_ns += cap.chunk.host_ts.saturating_sub(started);
        }
        let epoch = *epoch_ns.get_or_insert(cap.chunk.host_ts);
        let t0_s = cap
            .chunk
            .host_ts
            .saturating_sub(epoch)
            .saturating_sub(paused_ns) as f64
            / 1e9;
        let stream = cap.stream;
        let samples = cap.chunk.samples;
        // Hand the raw chunk to the recorder task first, on this always-drained path, so `audio.wav`
        // captures every *raw* chunk even when a stream's transcriber is wedged/behind. AEC applies
        // only to what live transcription sees — the archive stays raw, and the offline refine reads
        // only the Them channel. `try_send` never blocks demux; on a full recorder queue (a sustained
        // disk stall) drop-with-log — the recorder re-anchors on `t0_s`, so the drop is a silent gap,
        // not a desync.
        if let Some(tx) = rec_tx.as_ref() {
            match tx.try_send((samples.clone(), t0_s, stream)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    tracing::debug!("recorder queue full; dropping chunk")
                }
                Err(TrySendError::Closed(_)) => {}
            }
        }
        // Me is echo-cancelled against the Them tap; Them forwards unchanged and doubles as the
        // canceller's far-end reference. A Me chunk may not clean immediately (it briefly awaits the
        // reference), and a Them chunk can release previously-buffered Me — so both paths can yield
        // cleaned Me to forward.
        match stream {
            Stream::Them => {
                let ready = canceller.push_far(t0_s, &samples);
                them_fwd.forward(t0_s, samples);
                for (mt0, m) in ready {
                    me_fwd.forward(mt0, m);
                }
            }
            Stream::Me => {
                for (mt0, m) in canceller.process_me(t0_s, &samples) {
                    me_fwd.forward(mt0, m);
                }
            }
        }
    }
    me_fwd.log_summary();
    them_fwd.log_summary();
    // Capture ended: `rec_tx` drops as this task returns, so the recorder task drains its queue,
    // writes the tail, and finalizes `audio.wav` (awaited by `Pipeline::close`).
    // If capture ended without an intentional `close()` (the helper crashed / the media socket
    // EOF'd), signal it so the orchestrator finalizes the meeting rather than leaving it live with a
    // dead pipeline. On an intentional stop, `died_tx` drops here instead (Err on the receiver).
    if !intentional_stop.load(Ordering::SeqCst) {
        tracing::warn!("capture ended unexpectedly (helper crash / socket EOF)");
        let _ = died_tx.send(());
    }
}

/// Replace a dead sidecar, sleeping the transcriber's backoff before each attempt. `attempts` counts
/// restarts since the last sidecar that ran for [`RESPAWN_STABLE`]. Returns the new segment channel,
/// or `None` once the retry budget is spent.
async fn respawn_sidecar(
    role: StreamRole,
    transcriber: &mut dyn Transcriber,
    attempts: &mut usize,
    last_spawn: &mut Instant,
    mut reason: String,
    stats: Option<&LiveStats>,
) -> Option<mpsc::Receiver<SidecarSegment>> {
    if last_spawn.elapsed() >= RESPAWN_STABLE {
        *attempts = 0;
    }
    let backoff = transcriber.respawn_backoff().to_vec();
    loop {
        let Some(delay) = backoff.get(*attempts).copied() else {
            tracing::error!(
                stream = ?role,
                reason = %reason,
                "sidecar restart budget exhausted; live transcription for this stream has stopped",
            );
            return None;
        };
        *attempts += 1;
        tracing::warn!(stream = ?role, attempt = *attempts, reason = %reason, "restarting sidecar");
        tokio::time::sleep(delay).await;
        match transcriber.respawn().await {
            Ok(rx) => {
                *last_spawn = Instant::now();
                if let Some(stats) = stats {
                    let counter = match role {
                        StreamRole::Me => &stats.me_respawns,
                        StreamRole::Them => &stats.them_respawns,
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                return Some(rx);
            }
            Err(err) => reason = err.to_string(),
        }
    }
}

/// Per-stream task: feed the transcriber, persist its segments shifted by `offset`, and respawn a
/// sidecar that dies mid-meeting (re-basing `offset` to the replacement's first chunk).
#[allow(clippy::too_many_arguments)]
async fn stream_loop(
    role: StreamRole,
    mut transcriber: Box<dyn Transcriber>,
    mut chunk_rx: mpsc::Receiver<(f64, Vec<f32>)>,
    mut emit_rx: mpsc::Receiver<SidecarSegment>,
    pool: SqlitePool,
    meeting_id: Uuid,
    broadcast_tx: broadcast::Sender<String>,
    mut ane_ready: watch::Receiver<bool>,
    last_activity: Arc<Mutex<Instant>>,
    echo_dedup: Arc<Mutex<EchoDedup>>,
    stats: Option<Arc<LiveStats>>,
) {
    // Serialize live inference against the offline refine on the shared ANE permit: wait until this
    // meeting holds it before feeding the sidecar. Recording is unaffected (demux records on its own
    // path), so only live transcription waits — briefly, and only if a prior meeting's refine is
    // still finishing. `Err` means the permit holder was aborted at close; fall through to wind down
    // on the now-closed capture channel. Any chunks demux delivers meanwhile buffer in `chunk_rx`
    // (dropped-with-log only past its capacity), so a fast acquire loses nothing.
    let _ = ane_ready.wait_for(|&ready| ready).await;

    let mut offset: Option<f64> = None;
    let mut clusters: HashMap<i64, Uuid> = HashMap::new();
    // Added to a Them sidecar's speaker index, so a replacement diarizer starts on fresh ordinals.
    let mut ordinal_base: i64 = 0;
    let mut feeding = true;
    // True while a sidecar is attached; false once its restart budget is spent, after which chunks
    // are still drained (level meter, dead-mic watch) but not fed.
    let mut sidecar_live = true;
    let mut respawn_attempts: usize = 0;
    let mut last_spawn = Instant::now();
    let mut dead_mic = DeadMicMonitor::default();
    let mut level_meter = LevelMeter::new();
    // Samples fed to the sidecar so far (including any silence padding), so its sample-count
    // timeline can be kept aligned to meeting time.
    let mut fed_samples: u64 = 0;

    while feeding || sidecar_live {
        tokio::select! {
            // No `biased`: a chunk-first bias lets a producer that outpaces wall-clock (e.g. an
            // offline WAV replay) keep `chunk_rx` non-empty and starve `emit_rx`, which — with the
            // sidecar's stdout backpressure — can wedge the loop in `feed().await`. Fair (random)
            // polling gives the emit drain a turn even under a saturated chunk source.
            chunk = chunk_rx.recv(), if feeding => match chunk {
                Some((t0_s, samples)) => {
                    let base = *offset.get_or_insert(t0_s);
                    dead_mic.observe(role, &samples, &broadcast_tx);
                    level_meter.observe(role, &samples, &broadcast_tx);
                    if sidecar_live {
                        // Keep the sidecar's sample-count timeline aligned to meeting time: if this
                        // chunk's timestamp is past where the samples fed so far place it (dropped
                        // frames, a tap rebuild, or a chunk dropped by demux under backpressure), pad
                        // the gap with silence so the single `offset` mapping in `handle` stays correct
                        // and transcript times track `audio.wav` (which re-anchors on `t0_s` too).
                        let expected_s = fed_samples as f64 / SAMPLE_RATE;
                        let gap_s = (t0_s - base) - expected_s;
                        if gap_s > RESYNC_THRESHOLD_S {
                            let pad = ((gap_s * SAMPLE_RATE).round() as usize).min(MAX_SILENCE_PAD_SAMPLES);
                            tracing::debug!(role = ?role, gap_s, pad, "resync: padding sidecar timeline with silence");
                            fed_samples += pad as u64;
                            transcriber.feed(vec![0.0; pad]).await;
                        }
                        fed_samples += samples.len() as u64;
                        transcriber.feed(samples).await;
                        if transcriber.is_broken() && transcriber.can_respawn() {
                            // Keep what the dying sidecar already emitted, at its own offset.
                            while let Ok(seg) = emit_rx.try_recv() {
                                handle(role, &seg, offset.unwrap_or(0.0), &pool, meeting_id, &broadcast_tx, &mut clusters, ordinal_base, &echo_dedup).await;
                            }
                            match respawn_sidecar(role, transcriber.as_mut(), &mut respawn_attempts, &mut last_spawn, "stdin write failed".into(), stats.as_deref()).await {
                                Some(rx) => {
                                    emit_rx = rx;
                                    offset = None;
                                    fed_samples = 0;
                                    if role == StreamRole::Them {
                                        ordinal_base = next_ordinal_base(&pool, meeting_id, &clusters).await;
                                    }
                                }
                                None => {
                                    transcriber.close().await;
                                    sidecar_live = false;
                                }
                            }
                        }
                    }
                }
                // Capture ended: stop feeding and flush the sidecar's finalized tail. The emit
                // channel closes once the sidecar exits, ending the drain below.
                None => {
                    feeding = false;
                    transcriber.close().await;
                }
            },
            seg = emit_rx.recv(), if sidecar_live => match seg {
                Some(seg) => {
                    // Any emitted segment (partial or final, either stream) is VAD-gated speech, so it
                    // resets the silence clock the inactivity watchdog measures.
                    *last_activity.lock_recover() = Instant::now();
                    handle(
                        role,
                        &seg,
                        offset.unwrap_or(0.0),
                        &pool,
                        meeting_id,
                        &broadcast_tx,
                        &mut clusters,
                        ordinal_base,
                        &echo_dedup,
                    )
                    .await;
                }
                // Sidecar output closed: respawn it while capture still flows, else close (idempotent)
                // to reap the child.
                None => {
                    let recovered = if feeding && transcriber.can_respawn() {
                        respawn_sidecar(role, transcriber.as_mut(), &mut respawn_attempts, &mut last_spawn, "sidecar exited".into(), stats.as_deref()).await
                    } else {
                        None
                    };
                    match recovered {
                        Some(rx) => {
                            emit_rx = rx;
                            offset = None;
                            fed_samples = 0;
                            if role == StreamRole::Them {
                                ordinal_base = next_ordinal_base(&pool, meeting_id, &clusters).await;
                            }
                        }
                        None => {
                            transcriber.close().await;
                            sidecar_live = false;
                        }
                    }
                }
            },
        }
    }
}

/// A transcript event pushed to WebSocket subscribers. Field order + names are fixed by the wire
/// contract: `kind`, `stream`, `speaker_label`, `text`, `start_s`, `end_s`.
#[derive(Serialize)]
struct TranscriptEvent<'a> {
    kind: SegmentKind,
    stream: &'a str,
    speaker_label: &'a str,
    text: &'a str,
    start_s: f64,
    end_s: f64,
}

fn publish(broadcast_tx: &broadcast::Sender<String>, event: &TranscriptEvent<'_>) {
    // Skip the serialize entirely with no live subscribers (background recording, no view open). The
    // finals this drops are still persisted by `handle`, and a reconnecting client re-seeds from the DB.
    if broadcast_tx.receiver_count() == 0 {
        return;
    }
    if let Ok(line) = serde_json::to_string(event) {
        // Err just means no live subscribers, which is fine.
        let _ = broadcast_tx.send(line);
    }
}

/// A warm-up status event for WebSocket subscribers: `{"kind":"status","state":"ready"}`, broadcast
/// once the transcription sidecars finish loading so a client showing a "preparing" notice clears
/// it. (The initial `"warming"` snapshot is sent by the WS handler on connect; a distinct `kind`
/// keeps it off the transcript-line path.)
#[derive(Serialize)]
struct StatusEvent<'a> {
    kind: &'a str,
    state: &'a str,
}

fn publish_status(broadcast_tx: &broadcast::Sender<String>, state: &str) {
    if broadcast_tx.receiver_count() == 0 {
        return;
    }
    if let Ok(line) = serde_json::to_string(&StatusEvent {
        kind: "status",
        state,
    }) {
        let _ = broadcast_tx.send(line);
    }
}

/// The escalation stage the inactivity watchdog is in, given the current silence. Pure so the
/// escalation logic is unit-tested without a running pipeline.
#[derive(Debug, PartialEq, Eq)]
enum Stage {
    /// Below the prompt threshold, or already prompted this silence episode: do nothing.
    None,
    /// Crossed the prompt threshold this episode: nudge the UI.
    Prompt,
    /// Crossed the end threshold: auto-end the meeting.
    End,
}

/// Decide the watchdog's action from the elapsed silence, the thresholds, and whether this silence
/// episode was already prompted. Each stage is gated by its own toggle; the (enabled) `end_after`
/// wins over `prompt_after`, so a long-silent meeting auto-ends even if it was never prompted (e.g.
/// the prompt is disabled, or silence began before the first prompt window elapsed).
fn silence_stage(silence: Duration, cfg: &InactivityConfig, prompted: bool) -> Stage {
    if cfg.auto_end_enabled && silence >= cfg.end_after {
        Stage::End
    } else if cfg.prompt_enabled && silence >= cfg.prompt_after && !prompted {
        Stage::Prompt
    } else {
        Stage::None
    }
}

/// A "still recording?" prompt pushed to WebSocket subscribers:
/// `{"kind":"prompt","silent_seconds":N}`. Field names/order are fixed by the wire contract and
/// mirrored by `hearsay-core`'s `PromptEvent` schema for the TypeScript codegen.
#[derive(Serialize)]
struct PromptEvent<'a> {
    kind: &'a str,
    silent_seconds: u64,
}

/// A capture-health notice pushed to WebSocket subscribers:
/// `{"kind":"capture_health","stream":"me","state":"silent"}`.
#[derive(Serialize)]
struct CaptureHealthEvent<'a> {
    kind: &'a str,
    stream: &'a str,
    state: &'a str,
}

fn publish_capture_health(broadcast_tx: &broadcast::Sender<String>, state: &str) {
    if broadcast_tx.receiver_count() == 0 {
        return;
    }
    if let Ok(line) = serde_json::to_string(&CaptureHealthEvent {
        kind: "capture_health",
        stream: "me",
        state,
    }) {
        let _ = broadcast_tx.send(line);
    }
}

/// A capture-state notice pushed to WebSocket subscribers: `{"kind":"capture_state","state":"paused"}`
/// (or `"active"`). Lets a live view freeze the timer/waveform on pause and resume them; also snapshot
/// on connect so a reopened window reflects a mid-meeting pause.
#[derive(Serialize)]
struct CaptureStateEvent<'a> {
    kind: &'a str,
    state: &'a str,
}

fn publish_capture_state(broadcast_tx: &broadcast::Sender<String>, state: &str) {
    if broadcast_tx.receiver_count() == 0 {
        return;
    }
    if let Ok(line) = serde_json::to_string(&CaptureStateEvent {
        kind: "capture_state",
        state,
    }) {
        let _ = broadcast_tx.send(line);
    }
}

/// An audio-level notice pushed to WebSocket subscribers: `{"kind":"level","stream":"me","rms":0.1}`.
/// Ephemeral (never persisted or replayed) — it only drives the live input waveform.
#[derive(Serialize)]
struct LevelEvent<'a> {
    kind: &'a str,
    stream: &'a str,
    rms: f32,
}

fn publish_level(broadcast_tx: &broadcast::Sender<String>, role: StreamRole, rms: f32) {
    if broadcast_tx.receiver_count() == 0 {
        return;
    }
    let stream = match role {
        StreamRole::Me => "me",
        StreamRole::Them => "them",
    };
    if let Ok(line) = serde_json::to_string(&LevelEvent {
        kind: "level",
        stream,
        rms,
    }) {
        let _ = broadcast_tx.send(line);
    }
}

/// Accumulates a stream's samples between `level` broadcasts, emitting the interval's RMS amplitude at
/// most every [`LEVEL_INTERVAL`]. Reset (drained) on each emit so the value tracks recent audio.
struct LevelMeter {
    sum_sq: f64,
    count: u64,
    last_emit: Instant,
}

impl LevelMeter {
    fn new() -> Self {
        LevelMeter {
            sum_sq: 0.0,
            count: 0,
            last_emit: Instant::now(),
        }
    }

    /// Fold in one chunk; broadcast the accumulated RMS once the interval elapses.
    fn observe(
        &mut self,
        role: StreamRole,
        samples: &[f32],
        broadcast_tx: &broadcast::Sender<String>,
    ) {
        for &s in samples {
            self.sum_sq += (s as f64) * (s as f64);
        }
        self.count += samples.len() as u64;
        if self.last_emit.elapsed() >= LEVEL_INTERVAL {
            let rms = if self.count > 0 {
                (self.sum_sq / self.count as f64).sqrt() as f32
            } else {
                0.0
            };
            publish_level(broadcast_tx, role, rms);
            self.sum_sq = 0.0;
            self.count = 0;
            self.last_emit = Instant::now();
        }
    }
}

/// Watches the mic stream for *digital* silence — samples that are exactly zero, which a working
/// microphone never produces because even a quiet room has a noise floor. Sustained exact zeros mean
/// the signal path is dead (hardware mute, a stale endpoint, a driver that reports healthy frames of
/// nothing), and the danger is that this is invisible downstream: ASR does not return "nothing" for
/// silence, it hallucinates fluent text. Surfacing it is what turns confident nonsense into a
/// diagnosable "your mic is muted".
///
/// Only the mic is watched. The Them stream is loopback, where exact zeros are the *normal* state
/// whenever no audio is playing, so the same check there would fire constantly.
#[derive(Default)]
struct DeadMicMonitor {
    consecutive_zero_samples: u64,
    flagged: bool,
}

impl DeadMicMonitor {
    /// Fold in one chunk, emitting a notice on the transition into or out of digital silence.
    fn observe(
        &mut self,
        role: StreamRole,
        samples: &[f32],
        broadcast_tx: &broadcast::Sender<String>,
    ) {
        if role != StreamRole::Me {
            return;
        }
        if samples.iter().any(|s| *s != 0.0) {
            self.consecutive_zero_samples = 0;
            if self.flagged {
                self.flagged = false;
                tracing::info!("mic signal returned");
                publish_capture_health(broadcast_tx, "ok");
            }
            return;
        }
        self.consecutive_zero_samples += samples.len() as u64;
        if !self.flagged && self.consecutive_zero_samples >= DEAD_MIC_AFTER_SAMPLES {
            self.flagged = true;
            tracing::warn!(
                seconds = self.consecutive_zero_samples as f64 / SAMPLE_RATE,
                "mic is delivering digital silence — muted, or the endpoint is dead"
            );
            publish_capture_health(broadcast_tx, "silent");
        }
    }
}

fn publish_prompt(broadcast_tx: &broadcast::Sender<String>, silent_seconds: u64) {
    if broadcast_tx.receiver_count() == 0 {
        return;
    }
    if let Ok(line) = serde_json::to_string(&PromptEvent {
        kind: "prompt",
        silent_seconds,
    }) {
        let _ = broadcast_tx.send(line);
    }
}

/// The inactivity watchdog: on a coarse tick, measure the silence clock, nudge the UI once per
/// silence episode at `prompt_after`, and auto-end the meeting at `end_after`. On the end path it
/// writes a transcript marker (so the finalized meeting records why it stopped) and fires
/// `inactive_tx` — the orchestrator runs the normal `stop_meeting` from there — then exits. Speech or
/// a `keep_alive` reset drops the silence back and re-arms the prompt.
#[allow(clippy::too_many_arguments)]
async fn inactivity_watchdog(
    cfg: InactivityConfig,
    tick: Duration,
    last_activity: Arc<Mutex<Instant>>,
    meeting_start: Instant,
    prompt_tx: watch::Sender<Option<u64>>,
    broadcast_tx: broadcast::Sender<String>,
    pool: SqlitePool,
    meeting_id: Uuid,
    inactive_tx: oneshot::Sender<()>,
    paused: Arc<AtomicBool>,
) {
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut prompted = false;
    loop {
        ticker.tick().await;
        // A paused meeting is not idle — the user stepped away deliberately. Hold the silence clock at
        // now and clear any active prompt so a pause is never mistaken for inactivity and auto-ended.
        if paused.load(Ordering::SeqCst) {
            *last_activity.lock_recover() = Instant::now();
            if prompted {
                prompted = false;
                let _ = prompt_tx.send(None);
            }
            continue;
        }
        let silence = last_activity.lock_recover().elapsed();
        match silence_stage(silence, &cfg, prompted) {
            Stage::None => {
                // Speech (or a Keep-recording reset) dropped the silence below the prompt threshold:
                // clear any active prompt and re-arm for the next episode.
                if prompted && silence < cfg.prompt_after {
                    prompted = false;
                    let _ = prompt_tx.send(None);
                }
            }
            Stage::Prompt => {
                prompted = true;
                let secs = silence.as_secs();
                let _ = prompt_tx.send(Some(secs));
                publish_prompt(&broadcast_tx, secs);
            }
            Stage::End => {
                let t = meeting_start.elapsed().as_secs_f64();
                write_inactivity_marker(&pool, meeting_id, &broadcast_tx, cfg.end_after, t).await;
                let _ = inactive_tx.send(());
                return;
            }
        }
    }
}

/// Insert + broadcast a synthetic transcript line recording that the meeting auto-ended on silence.
/// Written on the Me stream with speaker "System" so the Them-only refine at stop preserves it and it
/// lands in `transcript.md`. Best-effort: a persist failure is logged, never blocks the auto-end.
async fn write_inactivity_marker(
    pool: &SqlitePool,
    meeting_id: Uuid,
    broadcast_tx: &broadcast::Sender<String>,
    end_after: Duration,
    t: f64,
) {
    let minutes = end_after.as_secs() / 60;
    let text = format!("[Recording auto-ended after {minutes} minutes of no speech detected]");
    if let Err(err) = queries::insert_segment(
        pool,
        meeting_id,
        StreamRole::Me.stream(),
        "System",
        &text,
        t,
        t,
        None,
    )
    .await
    {
        tracing::error!(error = %err, "failed to persist inactivity auto-end marker");
    }
    publish(
        broadcast_tx,
        &TranscriptEvent {
            kind: SegmentKind::Final,
            stream: "me",
            speaker_label: "System",
            text: text.as_str(),
            start_s: t,
            end_s: t,
        },
    );
}

/// Persist + broadcast one emitted segment, applying the stream's meeting-time `offset`.
#[allow(clippy::too_many_arguments)]
async fn handle(
    role: StreamRole,
    seg: &SidecarSegment,
    offset: f64,
    pool: &SqlitePool,
    meeting_id: Uuid,
    broadcast_tx: &broadcast::Sender<String>,
    clusters: &mut HashMap<i64, Uuid>,
    ordinal_base: i64,
    echo_dedup: &Arc<Mutex<EchoDedup>>,
) {
    let start_s = seg.start_s + offset;
    let end_s = seg.end_s + offset;

    match role {
        // Me is always the local speaker: broadcast partials + finals; persist only finals.
        StreamRole::Me => {
            // Drop a final that is an echo of concurrent Them speech leaking through the mic (the
            // text-level backstop behind acoustic AEC): no broadcast, no persist. Partials still
            // stream — they are ephemeral and the next one supersedes any echo that flashed live.
            if seg.kind == SegmentKind::Final
                && echo_dedup.lock_recover().is_echo(start_s, end_s, &seg.text)
            {
                tracing::debug!(text = %seg.text, start_s, end_s, "dropping Me final as echo of Them");
                return;
            }
            publish(
                broadcast_tx,
                &TranscriptEvent {
                    kind: seg.kind,
                    stream: "me",
                    speaker_label: "Me",
                    text: &seg.text,
                    start_s,
                    end_s,
                },
            );
            if seg.kind == SegmentKind::Final {
                if let Err(err) = queries::insert_segment(
                    pool,
                    meeting_id,
                    role.stream(),
                    "Me",
                    &seg.text,
                    start_s,
                    end_s,
                    None,
                )
                .await
                {
                    tracing::error!(error = %err, "failed to persist Me segment");
                }
            }
        }
        // Them: a partial is pre-diarization, so stream it speaker-less ("Them") and don't persist.
        // A final carries a 0-based speaker ordinal -> a `Speaker N` label + a per-meeting cluster.
        StreamRole::Them => {
            if seg.kind == SegmentKind::Partial {
                echo_dedup
                    .lock_recover()
                    .record_them_partial(start_s, end_s, &seg.text);
                publish(
                    broadcast_tx,
                    &TranscriptEvent {
                        kind: SegmentKind::Partial,
                        stream: "them",
                        speaker_label: "Them",
                        text: &seg.text,
                        start_s,
                        end_s,
                    },
                );
                return;
            }
            let ordinal = ordinal_base + seg.speaker.unwrap_or(0) + 1;
            let label = format!("Speaker {ordinal}");
            let cluster_id = cluster_for(pool, meeting_id, ordinal, clusters).await;
            if let Err(err) = queries::insert_segment(
                pool,
                meeting_id,
                role.stream(),
                &label,
                &seg.text,
                start_s,
                end_s,
                cluster_id,
            )
            .await
            {
                tracing::error!(error = %err, "failed to persist Them segment");
            }
            publish(
                broadcast_tx,
                &TranscriptEvent {
                    kind: SegmentKind::Final,
                    stream: "them",
                    speaker_label: &label,
                    text: &seg.text,
                    start_s,
                    end_s,
                },
            );
            // Record as a candidate echo source for the Me dedup backstop above.
            echo_dedup
                .lock_recover()
                .record_them(start_s, end_s, &seg.text);
        }
    }
}

/// Get-or-create the per-meeting cluster for a Them speaker ordinal (unlocked, no centroid live —
/// the offline refine re-seeds centroids). Returns `None` if the cluster row could not be created,
/// leaving the segment unbound.
async fn cluster_for(
    pool: &SqlitePool,
    meeting_id: Uuid,
    ordinal: i64,
    clusters: &mut HashMap<i64, Uuid>,
) -> Option<Uuid> {
    if let Some(id) = clusters.get(&ordinal) {
        return Some(*id);
    }
    match queries::create_cluster(pool, meeting_id, ordinal, false, None).await {
        Ok(cluster) => {
            clusters.insert(ordinal, cluster.id);
            Some(cluster.id)
        }
        Err(err) => {
            tracing::error!(error = %err, ordinal, "failed to create speaker cluster");
            None
        }
    }
}

/// The highest ordinal this meeting has used, live or in the database, so new ones never reuse it.
async fn next_ordinal_base(
    pool: &SqlitePool,
    meeting_id: Uuid,
    clusters: &HashMap<i64, Uuid>,
) -> i64 {
    let seen = clusters.keys().copied().max().unwrap_or(0);
    let stored = match queries::list_speaker_rows(pool, meeting_id).await {
        Ok(rows) => rows.iter().map(|r| r.ordinal).max().unwrap_or(0),
        Err(err) => {
            tracing::error!(error = %err, "failed to read speaker ordinals");
            0
        }
    };
    seen.max(stored)
}

#[cfg(test)]
mod tests {
    use super::{
        demux, inactivity_watchdog, silence_stage, DeadMicMonitor, EchoCanceller, InactivityConfig,
        LevelMeter, Stage, StreamRole, DEAD_MIC_AFTER_SAMPLES, LEVEL_INTERVAL,
    };
    use crate::types::{AudioChunk, CaptureChunk, Stream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Drain whatever the monitor published, as `(state)` strings.
    fn drain(rx: &mut tokio::sync::broadcast::Receiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(line) = rx.try_recv() {
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["kind"], "capture_health");
            assert_eq!(v["stream"], "me");
            out.push(v["state"].as_str().unwrap().to_string());
        }
        out
    }

    #[test]
    fn dead_mic_flags_only_after_sustained_digital_silence() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let mut monitor = DeadMicMonitor::default();

        // One second short of the threshold: still quiet, not yet a verdict.
        let almost = (DEAD_MIC_AFTER_SAMPLES - 16_000) as usize;
        monitor.observe(StreamRole::Me, &vec![0.0; almost], &tx);
        assert!(drain(&mut rx).is_empty());

        // Crossing it reports once, and staying silent does not repeat the notice.
        monitor.observe(StreamRole::Me, &vec![0.0; 16_000], &tx);
        assert_eq!(drain(&mut rx), vec!["silent"]);
        monitor.observe(StreamRole::Me, &vec![0.0; 160_000], &tx);
        assert!(drain(&mut rx).is_empty());

        // A single non-zero sample is signal returning — clears, and re-arms for the next episode.
        let mut recovered = vec![0.0; 1_000];
        recovered[500] = 0.01;
        monitor.observe(StreamRole::Me, &recovered, &tx);
        assert_eq!(drain(&mut rx), vec!["ok"]);
        monitor.observe(
            StreamRole::Me,
            &vec![0.0; DEAD_MIC_AFTER_SAMPLES as usize],
            &tx,
        );
        assert_eq!(drain(&mut rx), vec!["silent"]);
    }

    #[tokio::test]
    async fn demux_pause_elides_the_span_and_keeps_the_timeline_contiguous() {
        // Them chunks forward unchanged (no echo-canceller delay), so their forwarded t0_s is exactly
        // the meeting time demux computed — the cleanest way to observe the pause elision.
        let (cap_tx, cap_rx) = tokio::sync::mpsc::channel::<CaptureChunk>(64);
        let (me_tx, _me_rx) = tokio::sync::mpsc::channel::<(f64, Vec<f32>)>(64);
        let (them_tx, mut them_rx) = tokio::sync::mpsc::channel::<(f64, Vec<f32>)>(64);
        let paused = Arc::new(AtomicBool::new(false));
        let (died_tx, _died_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(demux(
            cap_rx,
            me_tx,
            them_tx,
            None,
            EchoCanceller::new(),
            Arc::new(AtomicBool::new(true)), // intentional_stop: suppress the death report on close
            paused.clone(),
            died_tx,
            None,
        ));

        let them = |host_ms: u64| CaptureChunk {
            stream: Stream::Them,
            chunk: AudioChunk {
                host_ts: host_ms * 1_000_000,
                samples: vec![0.1_f32; 1600],
            },
        };

        // Two active chunks anchor the epoch and run the timeline to 0.0 then 0.1. Awaiting each
        // forwarded chunk proves demux has processed it before we pause.
        cap_tx.send(them(0)).await.unwrap();
        assert!((them_rx.recv().await.unwrap().0 - 0.0).abs() < 1e-9);
        cap_tx.send(them(100)).await.unwrap();
        assert!((them_rx.recv().await.unwrap().0 - 0.1).abs() < 1e-9);

        // Pause, then feed chunks spanning 800 ms of wall time — all must be dropped (nothing
        // forwarded). paused is set before the sends, so demux always sees it when it reads them.
        paused.store(true, Ordering::SeqCst);
        cap_tx.send(them(200)).await.unwrap();
        cap_tx.send(them(300)).await.unwrap();
        // Let demux drain (and drop) the paused chunks before resuming, so the pause span is recorded.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            them_rx.try_recv().is_err(),
            "paused chunks must not be forwarded"
        );

        // Resume: the next chunk continues the timeline right after 0.1 (the 200 ms -> 1000 ms pause
        // span is elided), so its t0_s is 0.2 — contiguous, no gap.
        paused.store(false, Ordering::SeqCst);
        cap_tx.send(them(1000)).await.unwrap();
        let (t0, _) = them_rx.recv().await.unwrap();
        assert!(
            (t0 - 0.2).abs() < 1e-9,
            "resumed timeline should be contiguous (0.2), got {t0}"
        );

        drop(cap_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn demux_forwards_raw_chunks_to_the_recorder() {
        // With a recorder channel present, demux hands each raw chunk to it as (samples, t0_s, stream)
        // on the always-drained path (finding #4: audio.wav I/O moved off the demux thread).
        let (cap_tx, cap_rx) = tokio::sync::mpsc::channel::<CaptureChunk>(64);
        let (me_tx, _me_rx) = tokio::sync::mpsc::channel::<(f64, Vec<f32>)>(64);
        let (them_tx, _them_rx) = tokio::sync::mpsc::channel::<(f64, Vec<f32>)>(64);
        let (rec_tx, mut rec_rx) = tokio::sync::mpsc::channel::<(Vec<f32>, f64, Stream)>(64);
        let (died_tx, _died_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(demux(
            cap_rx,
            me_tx,
            them_tx,
            Some(rec_tx),
            EchoCanceller::new(),
            Arc::new(AtomicBool::new(true)), // intentional_stop: suppress the death report on close
            Arc::new(AtomicBool::new(false)),
            died_tx,
            None,
        ));

        // A Them chunk anchors the epoch at t0_s=0 and is recorded raw (Them is never echo-cancelled).
        cap_tx
            .send(CaptureChunk {
                stream: Stream::Them,
                chunk: AudioChunk {
                    host_ts: 0,
                    samples: vec![0.25_f32; 1600],
                },
            })
            .await
            .unwrap();

        let (samples, t0_s, stream) = rec_rx.recv().await.unwrap();
        assert!(matches!(stream, Stream::Them));
        assert!((t0_s - 0.0).abs() < 1e-9);
        assert_eq!(samples, vec![0.25_f32; 1600]);

        drop(cap_tx);
        let _ = handle.await;
    }

    #[test]
    fn level_meter_throttles_then_emits_rms() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let mut meter = LevelMeter::new();

        // Below the interval: accumulate but stay quiet.
        meter.observe(StreamRole::Me, &[0.5, 0.5, 0.5, 0.5], &tx);
        assert!(
            rx.try_recv().is_err(),
            "no level frame before the interval elapses"
        );

        // Once the interval has passed, the next chunk flushes the accumulated RMS as a `level` frame.
        meter.last_emit = Instant::now() - LEVEL_INTERVAL - Duration::from_millis(1);
        meter.observe(StreamRole::Me, &[1.0, 1.0], &tx);
        let v: serde_json::Value =
            serde_json::from_str(&rx.try_recv().expect("a level frame")).unwrap();
        assert_eq!(v["kind"], "level");
        assert_eq!(v["stream"], "me");
        // sqrt((4*0.25 + 2*1.0) / 6) = sqrt(0.5).
        assert!((v["rms"].as_f64().unwrap() - 0.5_f64.sqrt()).abs() < 1e-3);

        // Emitting resets the accumulators, so an immediate next chunk (interval not elapsed) is quiet.
        meter.observe(StreamRole::Me, &[0.9], &tx);
        assert!(
            rx.try_recv().is_err(),
            "accumulators reset and re-throttled after an emit"
        );
    }

    #[test]
    fn dead_mic_ignores_the_loopback_stream() {
        // Them is loopback: all-zero is the normal state whenever nothing is playing, so silence
        // there must never be reported as a fault.
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let mut monitor = DeadMicMonitor::default();
        monitor.observe(
            StreamRole::Them,
            &vec![0.0; DEAD_MIC_AFTER_SAMPLES as usize * 3],
            &tx,
        );
        assert!(rx.try_recv().is_err());
    }

    use chrono::Utc;
    use hearsay_db::queries;
    use hearsay_db::test_support::memory_pool;
    use tokio::sync::{broadcast, oneshot, watch};

    fn cfg() -> InactivityConfig {
        InactivityConfig {
            prompt_enabled: true,
            auto_end_enabled: true,
            prompt_after: Duration::from_secs(5 * 60),
            end_after: Duration::from_secs(10 * 60),
        }
    }

    #[test]
    fn no_stage_while_speech_is_recent() {
        // Well under the prompt threshold: nothing fires.
        assert_eq!(
            silence_stage(Duration::from_secs(60), &cfg(), false),
            Stage::None
        );
    }

    #[test]
    fn prompts_once_per_silence_episode() {
        let c = cfg();
        // Crossing the prompt threshold un-prompted fires the prompt...
        assert_eq!(
            silence_stage(Duration::from_secs(5 * 60), &c, false),
            Stage::Prompt
        );
        // ...but not again while still prompted and below the end threshold (no re-nudge spam).
        assert_eq!(
            silence_stage(Duration::from_secs(7 * 60), &c, true),
            Stage::None
        );
    }

    #[test]
    fn ends_at_the_end_threshold_regardless_of_prompt() {
        let c = cfg();
        // The end threshold wins even if the episode was never prompted (e.g. resumed mid-silence).
        assert_eq!(
            silence_stage(Duration::from_secs(10 * 60), &c, false),
            Stage::End
        );
        assert_eq!(
            silence_stage(Duration::from_secs(12 * 60), &c, true),
            Stage::End
        );
    }

    #[test]
    fn auto_end_disabled_never_ends_only_prompts() {
        // Prompt on, auto-end off: the prompt still fires, but silence past the end threshold never
        // auto-ends the meeting.
        let c = InactivityConfig {
            auto_end_enabled: false,
            ..cfg()
        };
        assert_eq!(
            silence_stage(Duration::from_secs(5 * 60), &c, false),
            Stage::Prompt
        );
        assert_eq!(
            silence_stage(Duration::from_secs(30 * 60), &c, true),
            Stage::None
        );
    }

    #[test]
    fn prompt_disabled_auto_ends_without_prompting() {
        // Prompt off, auto-end on: no prompt ever, but the meeting still auto-ends at the threshold.
        let c = InactivityConfig {
            prompt_enabled: false,
            ..cfg()
        };
        assert_eq!(
            silence_stage(Duration::from_secs(5 * 60), &c, false),
            Stage::None
        );
        assert_eq!(
            silence_stage(Duration::from_secs(10 * 60), &c, false),
            Stage::End
        );
    }

    /// Drive the watchdog end-to-end on a fast tick with never-reset silence: it broadcasts a
    /// `prompt` frame, then auto-ends — writing the transcript marker and firing the inactive signal.
    #[tokio::test]
    async fn watchdog_prompts_then_auto_ends_on_silence() {
        let pool = memory_pool().await;
        let meeting = queries::create_meeting(&pool, "T", "t", "/tmp/t", Utc::now())
            .await
            .unwrap();

        let (broadcast_tx, mut rx) = broadcast::channel::<String>(16);
        let (prompt_tx, _prompt_rx) = watch::channel(None::<u64>);
        let (inactive_tx, inactive_rx) = oneshot::channel();
        let now = Instant::now();
        let last_activity = Arc::new(Mutex::new(now));

        let watchdog = tokio::spawn(inactivity_watchdog(
            InactivityConfig {
                prompt_enabled: true,
                auto_end_enabled: true,
                prompt_after: Duration::ZERO,
                end_after: Duration::from_millis(40),
            },
            Duration::from_millis(5),
            last_activity,
            now,
            prompt_tx,
            broadcast_tx,
            pool.clone(),
            meeting.id,
            inactive_tx,
            Arc::new(AtomicBool::new(false)),
        ));

        // A prompt is broadcast before the auto-end.
        let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("a prompt frame should arrive")
            .unwrap();
        assert!(
            frame.contains(r#""kind":"prompt""#),
            "unexpected frame: {frame}"
        );

        // The auto-end fires, and the transcript gains the System marker.
        tokio::time::timeout(Duration::from_secs(2), inactive_rx)
            .await
            .expect("the inactive signal should fire")
            .expect("the watchdog should send, not drop, the signal");
        let _ = watchdog.await;

        let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
        assert!(
            segments
                .iter()
                .any(|s| s.speaker_label == "System" && s.text.contains("auto-ended")),
            "expected a System auto-end marker segment, got: {segments:?}"
        );
    }
}

#[cfg(test)]
mod drop_tests {
    use super::{DropStats, PcmForwarder, DROP_WARN_INTERVAL, PCM_CHANNEL_CAPACITY};
    use crate::types::Stream;
    use std::time::{Duration, Instant};

    #[test]
    fn channel_buffers_ten_seconds_of_nominal_chunks() {
        // 20 ms chunks: 50 per second.
        assert_eq!(PCM_CHANNEL_CAPACITY, 500);
    }

    #[test]
    fn forwarder_counts_chunks_and_samples_dropped_on_overflow() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(f64, Vec<f32>)>(2);
        let mut fwd = PcmForwarder::new(Stream::Me, tx);
        for i in 0..7 {
            fwd.forward(i as f64 * 0.02, vec![0.1; 320]);
        }
        // Two fit; the other five are dropped and counted.
        assert_eq!(fwd.stats.chunks, 5);
        assert_eq!(fwd.stats.samples, 5 * 320);
        assert_eq!(rx.try_recv().unwrap().0, 0.0);
        // Draining makes room again, and later chunks are not counted as drops.
        rx.try_recv().unwrap();
        fwd.forward(1.0, vec![0.1; 320]);
        assert_eq!(fwd.stats.chunks, 5);
    }

    #[test]
    fn forwarder_does_not_count_a_closed_channel_as_a_drop() {
        let (tx, rx) = tokio::sync::mpsc::channel::<(f64, Vec<f32>)>(2);
        drop(rx);
        let mut fwd = PcmForwarder::new(Stream::Them, tx);
        fwd.forward(0.0, vec![0.1; 320]);
        assert_eq!(fwd.stats.chunks, 0);
    }

    #[test]
    fn drop_warning_is_rate_limited_and_accumulates() {
        let mut stats = DropStats::default();
        let t0 = Instant::now();
        // The first drop warns at once.
        assert_eq!(stats.record(320, t0), Some((1, 320)));
        // A burst inside the interval stays quiet but is still counted.
        for i in 1..=100 {
            assert_eq!(stats.record(320, t0 + Duration::from_millis(i)), None);
        }
        assert_eq!(stats.chunks, 101);
        // The next warning after the interval reports everything dropped since the last one.
        assert_eq!(
            stats.record(320, t0 + DROP_WARN_INTERVAL),
            Some((101, 101 * 320))
        );
        assert_eq!(stats.samples, 102 * 320);
    }
}
