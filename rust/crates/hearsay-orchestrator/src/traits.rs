//! The two injected backends (capture + transcription) and the factory that builds them per
//! meeting. Kept behind traits so the [`Orchestrator`](crate::Orchestrator) lifecycle is testable
//! with fakes ([`crate::testing`]) before `hearsay-capture` / `hearsay-inference` exist.

use std::path::Path;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use hearsay_db::queries::{NotesResult, RefineResult};

use crate::error::OrchestratorError;
use crate::types::{CaptureChunk, SidecarSegment, Stream};

/// A per-OS audio capture source. Streams stream-tagged 16 kHz mono PCM on one monotonic clock;
/// the channel closes when capture stops.
#[async_trait]
pub trait AudioSource: Send {
    /// Begin capture and return the channel of stream-tagged chunks. Called once.
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError>;

    /// Stop capture (idempotent). Closes the channel returned by [`start`](Self::start).
    async fn stop(&mut self);
}

/// A streaming audio-AI sidecar for one stream (VAD/diarization + ASR): feed the stream's PCM in,
/// read the NDJSON segments it emits.
#[async_trait]
pub trait Transcriber: Send {
    /// Spawn the sidecar and return the channel of segments it emits. Called once. The channel is
    /// bounded (see `SEGMENT_CHANNEL_CAPACITY`): the producer awaits on a full channel so a stalled
    /// consumer backpressures the sidecar rather than growing memory without bound.
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError>;

    /// Feed one chunk of this stream's PCM (normalized mono `f32`). Takes ownership so the caller's
    /// buffer moves straight through to the sidecar/worker without a per-frame copy.
    async fn feed(&mut self, samples: Vec<f32>);

    /// Signal end-of-input and drain the sidecar's finalized tail. The segment channel closes once
    /// the sidecar exits.
    async fn close(&mut self);

    /// A one-shot that fires once this transcriber's sidecar has loaded its models and is serving,
    /// so the pipeline can tell the UI it is still warming up. `None` means already-ready — a
    /// pre-warmed sidecar or a fake with no load phase — which is the default. Called once, after
    /// [`start`](Self::start).
    fn ready_signal(&mut self) -> Option<oneshot::Receiver<()>> {
        None
    }
}

/// The post-meeting offline refine of the Them track. Behind a trait so the orchestrator does not
/// depend on `hearsay-inference` (whisper.cpp / cmake) and stays testable with fakes; the
/// production impl (in the `hearsay-core` binary) wraps `hearsay_inference::refine_audio_file`
/// (re-diarize via the Swift `hearsay-diarize` sidecar + re-transcribe with whisper).
#[async_trait]
pub trait Refiner: Send + Sync {
    /// Re-diarize + re-transcribe the Them channel of `audio_path` (the stereo `audio.wav`),
    /// returning the refined `Speaker N` segments + per-speaker voiceprints to persist in place of
    /// the live guesses.
    async fn refine(&self, audio_path: &Path) -> Result<RefineResult, OrchestratorError>;
}

/// The post-meeting local-LLM summarization of the finalized transcript. Behind a trait for the
/// same reason as [`Refiner`]: the orchestrator stays off the ML crates and testable with fakes.
/// The production impl (in `hearsay-backends`) spawns the out-of-process `hearsay-notes` sidecar.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Summarize the rendered speaker-attributed `transcript` into the note text (the model's reply
    /// verbatim; the prompt template dictates its shape).
    async fn summarize(&self, transcript: &str) -> Result<NotesResult, OrchestratorError>;
}

/// The capture + transcription backends for one meeting.
pub struct BackendInstance {
    pub source: Box<dyn AudioSource>,
    pub me: Box<dyn Transcriber>,
    pub them: Box<dyn Transcriber>,
}

/// Builds a fresh [`BackendInstance`] per `start_meeting`. The production backend spawns capture +
/// inference sidecars; tests inject a scripted one.
pub trait Backend: Send + Sync {
    /// Build the capture source + the Me/Them transcribers for one meeting.
    fn build(&self) -> BackendInstance;

    /// Whether the pre-warmed transcriber pair for the *next* meeting has finished loading its
    /// models. Lets the API gate "Start" until a meeting can actually transcribe. Defaults to `true`
    /// for backends without a warm pool (scripted test fakes have no load phase).
    fn sidecars_ready(&self) -> bool {
        true
    }

    /// Ensure the warm pool is (re)warming for the next meeting. Idempotent, and a no-op while a
    /// healthy pair is loading. Call only when it is *safe* to warm — after a meeting's sidecars are
    /// torn down, or while idle — never during a meeting, where a concurrent warm load would starve
    /// the live sidecars on the ANE. Default no-op for backends without a warm pool.
    fn ensure_pool_warm(&self) {}
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
