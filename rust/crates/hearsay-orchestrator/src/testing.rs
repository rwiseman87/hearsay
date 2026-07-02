//! Scripted fakes for driving the [`Orchestrator`](crate::Orchestrator) lifecycle without real
//! audio or model sidecars. A [`ScriptedBackend`] hands the pipeline a source that replays a fixed
//! list of chunks and transcribers that record what they were fed and emit a fixed list of segments
//! on close.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use hearsay_db::queries::RefinedThemSegment;

use crate::error::OrchestratorError;
use crate::traits::{AudioSource, Backend, BackendInstance, Refiner, Transcriber};
use crate::types::{CaptureChunk, SidecarSegment};

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
    tx: Option<mpsc::UnboundedSender<SidecarSegment>>,
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
    async fn start(
        &mut self,
    ) -> Result<mpsc::UnboundedReceiver<SidecarSegment>, OrchestratorError> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.tx = Some(tx);
        Ok(rx)
    }

    async fn feed(&mut self, samples: &[f32]) {
        self.fed.lock().unwrap().extend_from_slice(samples);
    }

    async fn close(&mut self) {
        if let Some(tx) = self.tx.take() {
            for seg in std::mem::take(&mut self.to_emit) {
                let _ = tx.send(seg);
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

/// A [`Refiner`] that yields a fixed set of refined segments (or a fixed error), ignoring the audio
/// file, and counts how many times it ran — for testing auto-refine-at-stop without whisper.
pub struct ScriptedRefiner {
    result: Result<Vec<RefinedThemSegment>, String>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedRefiner {
    /// A refiner that replaces the Them track with `segments` on each call. Returns it plus a
    /// shared call counter.
    pub fn new(segments: Vec<RefinedThemSegment>) -> (Arc<Self>, Arc<AtomicUsize>) {
        Self::from_result(Ok(segments))
    }

    /// A refiner that fails with `message` (to prove a refine error never fails the stop).
    pub fn failing(message: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
        Self::from_result(Err(message.to_string()))
    }

    fn from_result(
        result: Result<Vec<RefinedThemSegment>, String>,
    ) -> (Arc<Self>, Arc<AtomicUsize>) {
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
    async fn refine(
        &self,
        _audio_path: &Path,
    ) -> Result<Vec<RefinedThemSegment>, OrchestratorError> {
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
