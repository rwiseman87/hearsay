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

use hearsay_inference::{Punctuator, StreamEventKind, StreamingAsr, StreamingSession};
use hearsay_orchestrator::{OrchestratorError, SegmentKind, SidecarSegment, Transcriber};

/// Capacity of the PCM hand-off channel to the sherpa worker thread. Bounded so a worker that falls
/// behind real-time backpressures the caller (via the async `feed`) instead of growing without
/// bound; ~13 s of 100 ms chunks.
const PCM_CHANNEL_CAPACITY: usize = 128;

/// Capacity of the segment (`emit`) channel. Bounded (matching `ProcessTranscriber`) so a stalled
/// consumer backpressures the worker thread (via `blocking_send`) instead of growing without bound.
const SEGMENT_CHANNEL_CAPACITY: usize = 256;

/// A live transcriber driving one sherpa streaming session on a worker thread.
pub struct SherpaTranscriber {
    asr: StreamingAsr,
    /// Restores case + punctuation on the zipformer's bare uppercase output. `None` when the
    /// punctuation model is absent, which degrades to the raw uppercase text rather than failing.
    punct: Option<Punctuator>,
    pcm_tx: Option<mpsc::Sender<Vec<f32>>>,
    worker: Option<JoinHandle<()>>,
}

impl SherpaTranscriber {
    /// Wrap a loaded streaming recognizer (one per stream; the model is loaded by the caller).
    pub fn new(asr: StreamingAsr, punct: Option<Punctuator>) -> Self {
        Self {
            asr,
            punct,
            pcm_tx: None,
            worker: None,
        }
    }
}

fn to_segment(event: hearsay_inference::StreamEvent, punct: Option<&Punctuator>) -> SidecarSegment {
    SidecarSegment {
        kind: match event.kind {
            StreamEventKind::Partial => SegmentKind::Partial,
            StreamEventKind::Final => SegmentKind::Final,
        },
        text: match punct {
            Some(p) => p.restore(&event.text),
            None => event.text,
        },
        start_s: event.start_s,
        end_s: event.end_s,
        speaker: None, // live path is speaker-less; the offline refine assigns speakers at stop
    }
}

/// The worker: feed each PCM chunk through the session, forwarding events, then flush the tail.
fn run(
    mut session: StreamingSession,
    punct: Option<Punctuator>,
    mut pcm_rx: mpsc::Receiver<Vec<f32>>,
    seg_tx: mpsc::Sender<SidecarSegment>,
) {
    while let Some(chunk) = pcm_rx.blocking_recv() {
        for event in session.feed(&chunk) {
            // Blocking send from this dedicated worker thread: a full channel parks the worker
            // (backpressure) rather than growing unbounded.
            if seg_tx
                .blocking_send(to_segment(event, punct.as_ref()))
                .is_err()
            {
                return; // consumer dropped
            }
        }
    }
    for event in session.finish() {
        let _ = seg_tx.blocking_send(to_segment(event, punct.as_ref()));
    }
    // `seg_tx` drops here -> the segment channel closes, ending the orchestrator's stream task.
}

#[async_trait]
impl Transcriber for SherpaTranscriber {
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        let session = self.asr.session();
        let punct = self.punct.clone();
        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
        let (seg_tx, seg_rx) = mpsc::channel::<SidecarSegment>(SEGMENT_CHANNEL_CAPACITY);
        self.worker = Some(std::thread::spawn(move || {
            run(session, punct, pcm_rx, seg_tx)
        }));
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
