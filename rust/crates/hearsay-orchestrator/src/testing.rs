//! Scripted fakes for driving the [`Orchestrator`](crate::Orchestrator) lifecycle without real
//! audio or model sidecars. A [`ScriptedBackend`] hands the pipeline a source that replays a fixed
//! list of chunks and transcribers that record what they were fed and emit a fixed list of segments
//! on close.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Notify};

use hearsay_db::queries::{NotesResult, RefineResult, RefinedThemSegment};

use crate::error::OrchestratorError;
use crate::traits::{AudioSource, Backend, BackendInstance, Refiner, Summarizer, Transcriber};
use crate::transcriber::SEGMENT_CHANNEL_CAPACITY;
use crate::types::{AudioChunk, CaptureChunk, SegmentKind, SidecarSegment, Stream};

/// One stream-tagged PCM chunk on the capture clock.
pub fn chunk(stream: Stream, host_ts: u64, samples: &[f32]) -> CaptureChunk {
    CaptureChunk {
        stream,
        chunk: AudioChunk {
            host_ts,
            samples: samples.to_vec(),
        },
    }
}

/// One segment as a sidecar would emit it.
pub fn seg(
    kind: SegmentKind,
    text: &str,
    start_s: f64,
    end_s: f64,
    speaker: Option<i64>,
) -> SidecarSegment {
    SidecarSegment {
        kind,
        text: text.to_string(),
        start_s,
        end_s,
        speaker,
    }
}

/// A capture source that replays `chunks`, then holds the channel open (as a live capture would)
/// until [`stop`](AudioSource::stop) is called.
pub struct ScriptedSource {
    chunks: Vec<CaptureChunk>,
    stop_tx: Option<oneshot::Sender<()>>,
}

impl ScriptedSource {
    pub fn new(chunks: Vec<CaptureChunk>) -> Self {
        ScriptedSource {
            chunks,
            stop_tx: None,
        }
    }
}

#[async_trait]
impl AudioSource for ScriptedSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        let (tx, rx) = mpsc::channel(1024);
        let chunks = std::mem::take(&mut self.chunks);
        let (stop_tx, stop_rx) = oneshot::channel();
        self.stop_tx = Some(stop_tx);
        tokio::spawn(async move {
            for chunk in chunks {
                if tx.send(chunk).await.is_err() {
                    return;
                }
            }
            let _ = stop_rx.await; // stay open until stop(); then dropping `tx` closes the channel
        });
        Ok(rx)
    }

    async fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }
}

/// A transcriber that records every fed sample into `fed` and emits `to_emit` on
/// [`close`](Transcriber::close) (simulating a sidecar flushing its finalized tail).
pub struct ScriptedTranscriber {
    to_emit: Vec<SidecarSegment>,
    fed: Arc<Mutex<Vec<f32>>>,
    tx: Option<mpsc::Sender<SidecarSegment>>,
}

impl ScriptedTranscriber {
    pub fn new(to_emit: Vec<SidecarSegment>, fed: Arc<Mutex<Vec<f32>>>) -> Self {
        ScriptedTranscriber {
            to_emit,
            fed,
            tx: None,
        }
    }
}

#[async_trait]
impl Transcriber for ScriptedTranscriber {
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        let (tx, rx) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
        self.tx = Some(tx);
        Ok(rx)
    }

    async fn feed(&mut self, samples: Vec<f32>) {
        self.fed.lock().unwrap().extend_from_slice(&samples);
    }

    async fn close(&mut self) {
        if let Some(tx) = self.tx.take() {
            for seg in std::mem::take(&mut self.to_emit) {
                let _ = tx.send(seg).await;
            }
            // `tx` drops here -> the segment channel closes, ending the stream task.
        }
    }
}

/// Handles to a [`ScriptedBackend`]'s recorded feeds, for post-run assertions.
pub struct FedLogs {
    /// Every `f32` sample fed to the Me transcriber, in order.
    pub me: Arc<Mutex<Vec<f32>>>,
    /// Every `f32` sample fed to the Them transcriber, in order.
    pub them: Arc<Mutex<Vec<f32>>>,
}

struct Plan {
    chunks: Vec<CaptureChunk>,
    me_segments: Vec<SidecarSegment>,
    them_segments: Vec<SidecarSegment>,
    me_fed: Arc<Mutex<Vec<f32>>>,
    them_fed: Arc<Mutex<Vec<f32>>>,
}

/// A [`Backend`] whose single [`build`](Backend::build) hands out a [`ScriptedSource`] + two
/// [`ScriptedTranscriber`]s from a preset plan.
pub struct ScriptedBackend {
    plan: Mutex<Option<Plan>>,
}

impl ScriptedBackend {
    /// Build a backend that replays `chunks` and emits `me_segments` / `them_segments` on close.
    /// Returns the backend and the fed-sample logs (for asserting PCM routing).
    pub fn new(
        chunks: Vec<CaptureChunk>,
        me_segments: Vec<SidecarSegment>,
        them_segments: Vec<SidecarSegment>,
    ) -> (Arc<Self>, FedLogs) {
        let me_fed = Arc::new(Mutex::new(Vec::new()));
        let them_fed = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(ScriptedBackend {
            plan: Mutex::new(Some(Plan {
                chunks,
                me_segments,
                them_segments,
                me_fed: me_fed.clone(),
                them_fed: them_fed.clone(),
            })),
        });
        (
            backend,
            FedLogs {
                me: me_fed,
                them: them_fed,
            },
        )
    }
}

/// A [`Refiner`] that yields a fixed [`RefineResult`] (or a fixed error), ignoring the audio file,
/// and counts how many times it ran — for testing auto-refine-at-stop without the sidecar.
pub struct ScriptedRefiner {
    result: Result<RefineResult, String>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedRefiner {
    /// A refiner that replaces the Them track with `segments` (no voiceprints) on each call. Returns
    /// it plus a shared call counter.
    pub fn new(segments: Vec<RefinedThemSegment>) -> (Arc<Self>, Arc<AtomicUsize>) {
        Self::from_result(Ok(RefineResult {
            segments,
            ..Default::default()
        }))
    }

    /// A refiner that fails with `message` (to prove a refine error never fails the stop).
    pub fn failing(message: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
        Self::from_result(Err(message.to_string()))
    }

    fn from_result(result: Result<RefineResult, String>) -> (Arc<Self>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let refiner = Arc::new(ScriptedRefiner {
            result,
            calls: calls.clone(),
        });
        (refiner, calls)
    }
}

#[async_trait]
impl Refiner for ScriptedRefiner {
    async fn refine(&self, _audio_path: &Path) -> Result<RefineResult, OrchestratorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.result.clone().map_err(OrchestratorError::Backend)
    }
}

/// A [`Summarizer`] that yields a fixed [`NotesResult`] (or a fixed error), ignoring the transcript,
/// and counts how many times it ran — for testing notes generation + auto-at-stop without llama.cpp.
pub struct ScriptedSummarizer {
    result: Result<NotesResult, String>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedSummarizer {
    /// A summarizer that returns `content` (the verbatim note) on each call, plus a shared call counter.
    pub fn new(content: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
        Self::from_result(Ok(NotesResult {
            content: content.to_string(),
        }))
    }

    /// A summarizer that fails with `message` (to prove a notes error never fails the stop).
    pub fn failing(message: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
        Self::from_result(Err(message.to_string()))
    }

    fn from_result(result: Result<NotesResult, String>) -> (Arc<Self>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let summarizer = Arc::new(ScriptedSummarizer {
            result,
            calls: calls.clone(),
        });
        (summarizer, calls)
    }
}

#[async_trait]
impl Summarizer for ScriptedSummarizer {
    async fn summarize(&self, _transcript: &str) -> Result<NotesResult, OrchestratorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.result.clone().map_err(OrchestratorError::Backend)
    }
}

impl Backend for ScriptedBackend {
    fn build(&self) -> BackendInstance {
        let plan = self
            .plan
            .lock()
            .unwrap()
            .take()
            .expect("ScriptedBackend::build called more than once");
        BackendInstance {
            source: Box::new(ScriptedSource::new(plan.chunks)),
            me: Box::new(ScriptedTranscriber::new(plan.me_segments, plan.me_fed)),
            them: Box::new(ScriptedTranscriber::new(plan.them_segments, plan.them_fed)),
        }
    }
}

/// A [`Backend`] that builds a fresh empty instance (no chunks, no segments) on every call — for
/// lifecycle tests that start more than one meeting (e.g. a start that overlaps a prior refine),
/// which [`ScriptedBackend`] cannot serve because its single plan is consumed on first build.
pub struct EmptyBackend;

impl Backend for EmptyBackend {
    fn build(&self) -> BackendInstance {
        BackendInstance {
            source: Box::new(ScriptedSource::new(vec![])),
            me: Box::new(ScriptedTranscriber::new(
                vec![],
                Arc::new(Mutex::new(Vec::new())),
            )),
            them: Box::new(ScriptedTranscriber::new(
                vec![],
                Arc::new(Mutex::new(Vec::new())),
            )),
        }
    }
}

/// The canned Me/Them script a [`ProgressiveBackend`] replays: capture chunks to anchor the shared
/// clock, plus the timed Me/Them segments. Each segment's [`Duration`] is the gap *after the previous
/// emit on the same stream* before it is sent, so the transcript grows over the meeting.
#[derive(Clone)]
pub struct ProgressivePlan {
    pub chunks: Vec<CaptureChunk>,
    pub me: Vec<(Duration, SidecarSegment)>,
    pub them: Vec<(Duration, SidecarSegment)>,
}

/// A [`Transcriber`] that emits its segments *progressively during recording* — each after its gap
/// from the previous emit — rather than all at once on [`close`](Transcriber::close) like
/// [`ScriptedTranscriber`]. This is what lets a live consumer (the WebSocket push path, and thus the
/// browser E2E) watch the transcript populate while the meeting is still recording. `feed` is ignored
/// (the segments are scripted, not derived from the fed audio).
pub struct TimedTranscriber {
    to_emit: Vec<(Duration, SidecarSegment)>,
    tx: Option<mpsc::Sender<SidecarSegment>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TimedTranscriber {
    pub fn new(to_emit: Vec<(Duration, SidecarSegment)>) -> Self {
        TimedTranscriber {
            to_emit,
            tx: None,
            task: None,
        }
    }
}

#[async_trait]
impl Transcriber for TimedTranscriber {
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        let (tx, rx) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
        let emit_tx = tx.clone();
        let to_emit = std::mem::take(&mut self.to_emit);
        // Keep the original sender in `self` so the stream stays open (the meeting stays live) until
        // `close`; the spawned task emits on the clone and exits after the last segment.
        self.tx = Some(tx);
        self.task = Some(tokio::spawn(async move {
            for (gap, seg) in to_emit {
                tokio::time::sleep(gap).await;
                if emit_tx.send(seg).await.is_err() {
                    return; // the consumer went away (meeting stopped) — stop emitting
                }
            }
        }));
        Ok(rx)
    }

    async fn feed(&mut self, _samples: Vec<f32>) {}

    async fn close(&mut self) {
        // Abort any still-pending emits and drop the sender so the segment channel closes and the
        // pipeline's stream task ends — a real transcriber's `close` closes its stdout to the same
        // effect.
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.tx.take();
    }
}

/// A [`Backend`] whose transcribers emit their segments progressively over time (via
/// [`TimedTranscriber`]) so a live consumer sees the transcript grow *during* recording — the engine
/// behind the core's dev-only `HEARSAY_SCRIPTED` mode, which drives the browser E2E with no ANE/GPU.
/// Unlike [`ScriptedBackend`] it clones its plan on each [`build`](Backend::build), so it can serve
/// more than one meeting in a session (a stop-then-record-again flow).
pub struct ProgressiveBackend {
    plan: ProgressivePlan,
}

impl ProgressiveBackend {
    pub fn new(plan: ProgressivePlan) -> Self {
        ProgressiveBackend { plan }
    }
}

impl Backend for ProgressiveBackend {
    fn build(&self) -> BackendInstance {
        let plan = self.plan.clone();
        BackendInstance {
            source: Box::new(ScriptedSource::new(plan.chunks)),
            me: Box::new(TimedTranscriber::new(plan.me)),
            them: Box::new(TimedTranscriber::new(plan.them)),
        }
    }
}

/// A [`Transcriber`] whose `start` always fails — to prove a sidecar spawn failure tears the
/// already-started source/transcribers down and never strands a `recording` meeting row.
pub struct FailingTranscriber;

#[async_trait]
impl Transcriber for FailingTranscriber {
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        Err(OrchestratorError::Backend(
            "sidecar start failed (test)".into(),
        ))
    }

    async fn feed(&mut self, _samples: Vec<f32>) {}

    async fn close(&mut self) {}
}

/// A [`Backend`] whose `them` transcriber fails to start (source + `me` start fine), exercising the
/// pipeline's teardown-on-later-stage-failure guard and the orchestrator's start-failure cleanup.
pub struct FailingBackend;

impl Backend for FailingBackend {
    fn build(&self) -> BackendInstance {
        BackendInstance {
            source: Box::new(ScriptedSource::new(vec![])),
            me: Box::new(ScriptedTranscriber::new(
                vec![],
                Arc::new(Mutex::new(Vec::new())),
            )),
            them: Box::new(FailingTranscriber),
        }
    }
}

/// A [`Transcriber`] whose `feed` blocks until released (a sidecar that has stopped reading its
/// stdin), to prove a wedged transcriber stalls neither the recorder nor the other stream (demux
/// drops-with-log instead of blocking). Keeps its segment sender alive so the stream task stays
/// parked in `feed` rather than exiting via a closed emit channel; released so the task can wind
/// down cleanly at stop (no bounded-join abort needed for the test).
pub struct WedgingTranscriber {
    release: Arc<Notify>,
    released: bool,
    _tx: Option<mpsc::Sender<SidecarSegment>>,
}

#[async_trait]
impl Transcriber for WedgingTranscriber {
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        let (tx, rx) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
        self._tx = Some(tx);
        Ok(rx)
    }

    async fn feed(&mut self, _samples: Vec<f32>) {
        if !self.released {
            self.release.notified().await;
            self.released = true;
        }
    }

    async fn close(&mut self) {
        // Drop the segment sender so the emit channel closes and the stream task can exit (a real
        // transcriber's close() closes its stdout, which does the same).
        self._tx.take();
    }
}

/// Replay plan for [`WedgeMeBackend`]: the chunks, the Them fed-log, and the release handle.
type WedgePlan = (Vec<CaptureChunk>, Arc<Mutex<Vec<f32>>>, Arc<Notify>);

/// A [`Backend`] whose `me` transcriber wedges (blocks in `feed` until released) while `them`
/// records normally, for the head-of-line test: a wedged stream must not starve the other stream.
/// Exposes the Them fed-log and the release handle.
pub struct WedgeMeBackend {
    plan: Mutex<Option<WedgePlan>>,
}

impl WedgeMeBackend {
    /// Build a backend that replays `chunks` (Me wedges, Them records), plus the Them fed-log and
    /// the Notify that releases the wedged Me `feed`.
    pub fn new(chunks: Vec<CaptureChunk>) -> (Arc<Self>, Arc<Mutex<Vec<f32>>>, Arc<Notify>) {
        let them_fed = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let backend = Arc::new(WedgeMeBackend {
            plan: Mutex::new(Some((chunks, them_fed.clone(), release.clone()))),
        });
        (backend, them_fed, release)
    }
}

impl Backend for WedgeMeBackend {
    fn build(&self) -> BackendInstance {
        let (chunks, them_fed, release) = self
            .plan
            .lock()
            .unwrap()
            .take()
            .expect("WedgeMeBackend::build called more than once");
        BackendInstance {
            source: Box::new(ScriptedSource::new(chunks)),
            me: Box::new(WedgingTranscriber {
                release,
                released: false,
                _tx: None,
            }),
            them: Box::new(ScriptedTranscriber::new(vec![], them_fed)),
        }
    }
}

/// A [`Transcriber`] that reports a cold sidecar: its [`ready_signal`](Transcriber::ready_signal)
/// resolves only when the test fires the paired sender, so the warm-up state can be driven
/// deterministically. Otherwise a no-op (no segments, feed ignored).
pub struct WarmingTranscriber {
    ready_rx: Option<oneshot::Receiver<()>>,
    tx: Option<mpsc::Sender<SidecarSegment>>,
}

#[async_trait]
impl Transcriber for WarmingTranscriber {
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        let (tx, rx) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
        self.tx = Some(tx);
        Ok(rx)
    }

    async fn feed(&mut self, _samples: Vec<f32>) {}

    async fn close(&mut self) {
        self.tx.take();
    }

    fn ready_signal(&mut self) -> Option<oneshot::Receiver<()>> {
        self.ready_rx.take()
    }
}

/// A [`Backend`] whose two transcribers start cold (each exposes a ready signal), for testing the
/// pipeline's warm-up state. [`new`](Self::new) returns the backend plus the two senders that fire
/// the Me/Them ready signals.
pub struct WarmingBackend {
    plan: Mutex<Option<(oneshot::Receiver<()>, oneshot::Receiver<()>)>>,
}

impl WarmingBackend {
    pub fn new() -> (Arc<Self>, oneshot::Sender<()>, oneshot::Sender<()>) {
        let (me_tx, me_rx) = oneshot::channel();
        let (them_tx, them_rx) = oneshot::channel();
        let backend = Arc::new(WarmingBackend {
            plan: Mutex::new(Some((me_rx, them_rx))),
        });
        (backend, me_tx, them_tx)
    }
}

impl Backend for WarmingBackend {
    fn build(&self) -> BackendInstance {
        let (me_ready, them_ready) = self
            .plan
            .lock()
            .unwrap()
            .take()
            .expect("WarmingBackend::build called more than once");
        BackendInstance {
            source: Box::new(ScriptedSource::new(vec![])),
            me: Box::new(WarmingTranscriber {
                ready_rx: Some(me_ready),
                tx: None,
            }),
            them: Box::new(WarmingTranscriber {
                ready_rx: Some(them_ready),
                tx: None,
            }),
        }
    }
}

/// An [`AudioSource`] that replays `chunks` then **closes its channel on its own** (dropping the
/// sender) without waiting for [`stop`](AudioSource::stop) — simulating a helper crash / media
/// socket EOF, to exercise the orchestrator's capture-death supervisor.
pub struct CrashingSource {
    chunks: Vec<CaptureChunk>,
}

impl CrashingSource {
    pub fn new(chunks: Vec<CaptureChunk>) -> Self {
        CrashingSource { chunks }
    }
}

#[async_trait]
impl AudioSource for CrashingSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        let (tx, rx) = mpsc::channel(1024);
        let chunks = std::mem::take(&mut self.chunks);
        tokio::spawn(async move {
            for chunk in chunks {
                if tx.send(chunk).await.is_err() {
                    return;
                }
            }
            // Drop `tx` here (no wait for stop): the capture channel closes as if the helper died.
        });
        Ok(rx)
    }

    async fn stop(&mut self) {}
}

/// A [`Backend`] whose source crashes (closes capture on its own) after replaying `chunks`, for the
/// capture-death supervisor test.
pub struct CrashingBackend {
    plan: Mutex<Option<Vec<CaptureChunk>>>,
}

impl CrashingBackend {
    pub fn new(chunks: Vec<CaptureChunk>) -> Self {
        CrashingBackend {
            plan: Mutex::new(Some(chunks)),
        }
    }
}

impl Backend for CrashingBackend {
    fn build(&self) -> BackendInstance {
        let chunks = self
            .plan
            .lock()
            .unwrap()
            .take()
            .expect("CrashingBackend::build called more than once");
        BackendInstance {
            source: Box::new(CrashingSource::new(chunks)),
            me: Box::new(ScriptedTranscriber::new(
                vec![],
                Arc::new(Mutex::new(Vec::new())),
            )),
            them: Box::new(ScriptedTranscriber::new(
                vec![],
                Arc::new(Mutex::new(Vec::new())),
            )),
        }
    }
}

/// Handles for driving + observing a [`GateRefiner`] from a test.
pub struct GateHandle {
    /// Notified when `refine` begins (proves stop returned before the refine finished).
    pub started: Arc<Notify>,
    /// Notify to unblock the in-flight `refine` so it can complete.
    pub release: Arc<Notify>,
    /// How many times `refine` ran.
    pub calls: Arc<AtomicUsize>,
}

/// A [`Refiner`] that, on `refine`, signals it started and then blocks until released — for
/// asserting a new meeting can start while a previous one is still refining (i.e. the refine runs
/// off the op-lock). Yields an empty [`RefineResult`], so it replaces nothing.
pub struct GateRefiner {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

impl GateRefiner {
    /// A gated refiner plus the handle a test uses to observe its start and release it.
    pub fn new() -> (Arc<Self>, GateHandle) {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let refiner = Arc::new(GateRefiner {
            started: started.clone(),
            release: release.clone(),
            calls: calls.clone(),
        });
        (
            refiner,
            GateHandle {
                started,
                release,
                calls,
            },
        )
    }
}

#[async_trait]
impl Refiner for GateRefiner {
    async fn refine(&self, _audio_path: &Path) -> Result<RefineResult, OrchestratorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Ok(RefineResult::default())
    }
}
