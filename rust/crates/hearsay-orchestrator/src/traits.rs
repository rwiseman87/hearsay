//! The two injected backends (capture + transcription) and the factory that builds them per
//! meeting. Kept behind traits so the [`Orchestrator`](crate::Orchestrator) lifecycle is testable
//! with fakes ([`crate::testing`]) before `hearsay-capture` / `hearsay-inference` exist.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::OrchestratorError;
use crate::types::{CaptureChunk, SidecarSegment, Stream};

/// A per-OS audio capture source. Streams stream-tagged 16 kHz mono PCM on one monotonic clock;
/// the channel closes when capture stops. Port of the Python `Capture` / media channel.
#[async_trait]
pub trait AudioSource: Send {
    /// Begin capture and return the channel of stream-tagged chunks. Called once.
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError>;

    /// Stop capture (idempotent). Closes the channel returned by [`start`](Self::start).
    async fn stop(&mut self);
}

/// A streaming audio-AI sidecar for one stream (VAD/diarization + ASR). Port of the Python
/// `LiveSidecarProcessor`: feed the stream's PCM in, read the NDJSON segments it emits.
#[async_trait]
pub trait Transcriber: Send {
    /// Spawn the sidecar and return the channel of segments it emits. Called once.
    async fn start(&mut self)
        -> Result<mpsc::UnboundedReceiver<SidecarSegment>, OrchestratorError>;

    /// Feed one chunk of this stream's PCM (normalized mono `f32`).
    async fn feed(&mut self, samples: &[f32]);

    /// Signal end-of-input and drain the sidecar's finalized tail. The segment channel closes once
    /// the sidecar exits.
    async fn close(&mut self);
}

/// The capture + transcription backends for one meeting.
pub struct BackendInstance {
    pub source: Box<dyn AudioSource>,
    pub me: Box<dyn Transcriber>,
    pub them: Box<dyn Transcriber>,
}

/// Builds a fresh [`BackendInstance`] per `start_meeting` (the Rust analogue of the Python
/// `capture_factory` + pipeline factory). The production backend spawns capture + inference
/// sidecars; tests inject a scripted one.
pub trait Backend: Send + Sync {
    /// Build the capture source + the Me/Them transcribers for one meeting.
    fn build(&self) -> BackendInstance;
}

/// Which stream a transcriber handles, and thus how its segments are labeled + persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamRole {
    /// The local mic: always "Me", never diarized.
    Me,
    /// System audio: diarized into `Speaker N` clusters.
    Them,
}

impl StreamRole {
    pub(crate) fn stream(self) -> Stream {
        match self {
            StreamRole::Me => Stream::Me,
            StreamRole::Them => Stream::Them,
        }
    }
}
