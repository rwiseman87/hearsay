//! [`SherpaTranscriber`]: the pure-Rust live transcriber — the orchestrator [`Transcriber`] backed
//! by `hearsay-inference`'s streaming ASR instead of a Swift sidecar. This is the non-Mac (Windows)
//! live path: each stream's PCM drives a sherpa [`StreamingSession`], whose partial/final
//! [`StreamEvent`]s become speaker-less [`SidecarSegment`]s (the offline refine assigns speakers at
//! stop). Lives here because it needs both `hearsay-orchestrator` (the trait) and `hearsay-inference`
//! (the ASR); neither can depend on the other.
//!
//! sherpa decoding is blocking CPU work, so the session runs on a dedicated OS thread: PCM chunks
//! arrive over a std channel, segments leave over a tokio channel (the orchestrator drains it until
//! it closes at end-of-input).

use std::thread::JoinHandle;

use async_trait::async_trait;
use tokio::sync::mpsc;

use hearsay_inference::{StreamEventKind, StreamingAsr, StreamingSession};
use hearsay_orchestrator::{OrchestratorError, SegmentKind, SidecarSegment, Transcriber};

/// Capacity of the PCM hand-off channel to the sherpa worker thread. Bounded so a worker that falls
/// behind real-time backpressures the caller (via the async `feed`) instead of growing without
/// bound; ~13 s of 100 ms chunks.
const PCM_CHANNEL_CAPACITY: usize = 128;

/// A live transcriber driving one sherpa streaming session on a worker thread.
pub struct SherpaTranscriber {
    asr: StreamingAsr,
    pcm_tx: Option<mpsc::Sender<Vec<f32>>>,
    worker: Option<JoinHandle<()>>,
}

impl SherpaTranscriber {
    /// Wrap a loaded streaming recognizer (one per stream; the model is loaded by the caller).
    pub fn new(asr: StreamingAsr) -> Self {
        Self {
            asr,
            pcm_tx: None,
            worker: None,
        }
    }
}

fn to_segment(event: hearsay_inference::StreamEvent) -> SidecarSegment {
    SidecarSegment {
        kind: match event.kind {
            StreamEventKind::Partial => SegmentKind::Partial,
            StreamEventKind::Final => SegmentKind::Final,
        },
        text: event.text,
        start_s: event.start_s,
        end_s: event.end_s,
        speaker: None, // live path is speaker-less; the offline refine assigns speakers at stop
    }
}

/// The worker: feed each PCM chunk through the session, forwarding events, then flush the tail.
fn run(
    mut session: StreamingSession,
    mut pcm_rx: mpsc::Receiver<Vec<f32>>,
    seg_tx: mpsc::UnboundedSender<SidecarSegment>,
) {
    while let Some(chunk) = pcm_rx.blocking_recv() {
        for event in session.feed(&chunk) {
            if seg_tx.send(to_segment(event)).is_err() {
                return; // consumer dropped
            }
        }
    }
    for event in session.finish() {
        let _ = seg_tx.send(to_segment(event));
    }
    // `seg_tx` drops here -> the segment channel closes, ending the orchestrator's stream task.
}

#[async_trait]
impl Transcriber for SherpaTranscriber {
    async fn start(
        &mut self,
    ) -> Result<mpsc::UnboundedReceiver<SidecarSegment>, OrchestratorError> {
        let session = self.asr.session();
        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
        let (seg_tx, seg_rx) = mpsc::unbounded_channel::<SidecarSegment>();
        self.worker = Some(std::thread::spawn(move || run(session, pcm_rx, seg_tx)));
        self.pcm_tx = Some(pcm_tx);
        Ok(seg_rx)
    }

    async fn feed(&mut self, samples: Vec<f32>) {
        if let Some(tx) = &self.pcm_tx {
            let _ = tx.send(samples).await;
        }
    }

    async fn close(&mut self) {
        self.pcm_tx = None; // drop the sender -> the worker sees end-of-input, flushes, and exits
        if let Some(worker) = self.worker.take() {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }
}
