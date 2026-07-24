//! Orchestration: the live-capture engine behind `hearsay-core`'s meeting lifecycle + transcript
//! WebSocket. It creates the meeting row + folder, drives an [`AudioSource`], routes each stream's
//! 16 kHz PCM to its [`Transcriber`], and persists + broadcasts the partial/final segments the
//! transcribers emit.
//!
//! The [`Orchestrator`] implements the [`hearsay_engine::LiveEngine`] seam, so wiring it into
//! `hearsay-core` replaces the built-in `DisabledEngine`.
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
//! The pipeline records one timeline-accurate stereo `audio.wav` per meeting (Me=L / Them=R) when
//! recording is enabled, and writes `transcript.md` + `meeting.json` at stop. When a [`Refiner`] is
//! wired, [`Orchestrator::stop_meeting`] auto-runs the post-meeting refine (re-diarize +
//! re-transcribe the Them track, replacing the live guesses) before writing the transcript —
//! best-effort, so a missing recording or a refine error never fails the stop. See
//! `docs/architecture.md`.

mod aec;
mod error;
mod lock;
mod markdown;
mod orchestrator;
mod pipeline;
mod recorder;
mod traits;
mod transcriber;
mod types;
mod wav_source;

pub mod testing;

pub use error::OrchestratorError;
pub use hearsay_db::queries::{NotesResult, RefineResult, RefinedThemSegment};
pub use markdown::write_meeting_files;
pub use orchestrator::Orchestrator;
pub use traits::{AudioSource, Backend, BackendInstance, Refiner, Summarizer, Transcriber};
pub use transcriber::ProcessTranscriber;
pub use types::{AudioChunk, CaptureChunk, SegmentKind, SidecarSegment, Stream};
pub use wav_source::WavFileSource;
