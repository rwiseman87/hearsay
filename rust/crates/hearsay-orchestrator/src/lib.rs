//! Orchestration: the live-capture engine behind `hearsay-core`'s meeting lifecycle. [`Orchestrator`]
//! implements the [`hearsay_engine::LiveEngine`] seam, replacing the built-in `DisabledEngine`.
//!
//! [`AudioSource`] and [`Transcriber`] are traits so the whole lifecycle is testable without real
//! audio or model sidecars — [`WavFileSource`] replays a recorded `audio.wav`, and [`testing`] has
//! scripted fakes. See `docs/architecture.md` for the wiring and `docs/pipeline.md` for the stages.

mod aec;
mod echo_dedup;
mod error;
mod lock;
mod markdown;
mod orchestrator;
mod pipeline;
mod recorder;
mod traits;
mod transcriber;
mod tuning;
mod types;
mod wav_source;

pub mod testing;

pub use aec::{AecConfig, EchoCanceller};
pub use echo_dedup::EchoDedupConfig;
pub use error::OrchestratorError;
pub use hearsay_db::models::RefineGap;
pub use hearsay_db::queries::{NotesResult, RefineCoverage, RefineResult, RefinedThemSegment};
pub use markdown::write_meeting_files;
pub use orchestrator::{Defaults, Orchestrator};
pub use traits::{AudioSource, Backend, BackendInstance, Refiner, Summarizer, Transcriber};
pub use transcriber::{ProcessTranscriber, SEGMENT_CHANNEL_CAPACITY};
pub use tuning::{EchoDrop, LiveStats, LiveTuning};
pub use types::{AudioChunk, CaptureChunk, SegmentKind, SidecarSegment, Stream};
pub use wav_source::WavFileSource;
