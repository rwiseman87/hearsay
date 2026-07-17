//! Local-only inference for Hearsay.
//!
//! Being built Mac-first, smallest-verifiable-first: the offline ASR path is
//! implemented — [`WhisperAsr`] loads a GGML whisper model and transcribes 16 kHz mono audio into
//! timestamped segments (whisper.cpp via `whisper-rs`; CPU by default, GPU accel — Metal / Vulkan /
//! CUDA — is a `whisper-rs` Cargo feature). This is the accuracy-verification harness and the
//! orchestrator's post-meeting refine.
//!
//! Still to come (see the crate stub notes / `docs/architecture-cross-platform.md`): offline
//! diarization (Silero VAD + a speaker-embedding ONNX model) with WER/DER scoring, then the
//! streaming `Transcriber` sidecar for live captions.

mod asr;
mod audio;
mod diarizer;
mod error;
// The prompt/parse helpers are exercised by tests and by the `notes` feature's llama.cpp call; in a
// plain build without either they are legitimately unused, so allow it there rather than gate each fn.
#[cfg_attr(not(any(feature = "notes", test)), allow(dead_code))]
mod notes;
mod refine;
#[cfg(feature = "sherpa")]
mod sherpa_diarize;
#[cfg(feature = "sherpa")]
mod sherpa_streaming;

pub use asr::{AsrSegment, WhisperAsr, DEFAULT_LANGUAGE};
pub use audio::{read_them_channel, read_wav_mono_16k, SAMPLE_RATE};
pub use diarizer::{DiarTurn, Diarization, Diarizer};
pub use error::InferenceError;
#[cfg(feature = "notes")]
pub use notes::summarize;
pub use notes::MeetingNotes;
pub use notes::DEFAULT_NOTES_PROMPT;
pub use refine::{
    refine_audio_file, refine_them, refine_them_with, RefineOutput, RefinedSegment, SwiftDiarizer,
};
#[cfg(feature = "sherpa")]
pub use sherpa_diarize::{DiarizeTuning, SherpaDiarizer};
#[cfg(feature = "sherpa")]
pub use sherpa_streaming::{
    StreamEvent, StreamEventKind, StreamingAsr, StreamingModel, StreamingSession,
};
