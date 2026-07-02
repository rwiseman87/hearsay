//! Orchestration: the live-capture engine behind `hearsay-core`'s meeting lifecycle + transcript
//! WebSocket. It creates the meeting row + folder, drives an [`AudioSource`], routes each stream's
//! 16 kHz PCM to its [`Transcriber`], and persists + broadcasts the partial/final segments the
//! transcribers emit.
//!
//! Rust port of `src/hearsay/transcript/` (the `SessionManager` + pipeline + sidecar processors)
//! and `src/hearsay/helper/supervisor.py`. The [`Orchestrator`] implements the
//! [`hearsay_engine::LiveEngine`] seam, so wiring it into `hearsay-core` replaces the built-in
//! `DisabledEngine`.
//!
//! The two external backends are behind traits so the whole lifecycle is testable without real
//! audio or model sidecars:
//! - [`AudioSource`] — capture (per-OS: WASAPI loopback / Core Audio tap), provided later by
//!   `hearsay-capture`. [`WavFileSource`] is a file-backed source for offline / dev runs (replay a
//!   recorded `audio.wav` through the pipeline without hardware).
//! - [`Transcriber`] — a streaming VAD/diarization + ASR sidecar, provided later by
//!   `hearsay-inference`. [`ProcessTranscriber`] is the real `tokio::process` implementation
//!   (faithful to `live_base.py`); [`testing`] has scripted fakes.
//!
//! Deferred (tracked in `docs/TODO.md`, gated on `hearsay-capture` / `hearsay-inference`): the
//! stereo `audio.wav` recorder, the `transcript.md` markdown sink, and the offline refine at stop.
//! Finals persist to the database (the API's source of truth) today. See
//! `docs/architecture-cross-platform.md`.

mod error;
mod orchestrator;
mod pipeline;
mod traits;
mod transcriber;
mod types;
mod wav_source;

pub mod testing;

pub use error::OrchestratorError;
pub use orchestrator::Orchestrator;
pub use traits::{AudioSource, Backend, BackendInstance, Transcriber};
pub use transcriber::ProcessTranscriber;
pub use types::{AudioChunk, CaptureChunk, SegmentKind, SidecarSegment, Stream};
pub use wav_source::WavFileSource;
