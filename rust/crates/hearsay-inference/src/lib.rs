//! Local-only inference for Hearsay: the offline whisper ASR/refine and the cross-platform sherpa
//! live/diarize path.
//!
//! [`WhisperAsr`] loads a GGML whisper model and transcribes 16 kHz mono audio into timestamped
//! segments (whisper.cpp via `whisper-rs`; CPU by default, GPU accel — Metal / Vulkan / CUDA — is a
//! `whisper-rs` Cargo feature); it backs the orchestrator's post-meeting refine. Behind the
//! `sherpa` feature, [`StreamingAsr`] and [`SherpaDiarizer`] provide the pure-Rust live and offline
//! diarization used by the Windows backend.

mod asr;
mod audio;
mod diarizer;
mod error;
mod refine;
#[cfg(feature = "sherpa")]
mod sherpa_diarize;
#[cfg(feature = "sherpa")]
mod sherpa_punct;
#[cfg(feature = "sherpa")]
mod sherpa_streaming;

pub use asr::{AsrSegment, WhisperAsr, DEFAULT_LANGUAGE};
pub use audio::{read_them_channel, read_wav_mono_16k, SAMPLE_RATE};
pub use diarizer::{DiarTurn, Diarization, Diarizer};
pub use error::InferenceError;
pub use refine::{
    refine_audio_file, refine_audio_file_with, refine_them, refine_them_with, RefineOutput,
    RefinedSegment, SwiftDiarizer,
};
#[cfg(feature = "sherpa")]
pub use sherpa_diarize::{DiarizeTuning, SherpaDiarizer};
#[cfg(feature = "sherpa")]
pub use sherpa_punct::{PunctuationModel, Punctuator};
#[cfg(feature = "sherpa")]
pub use sherpa_streaming::{
    StreamEvent, StreamEventKind, StreamingAsr, StreamingModel, StreamingSession,
};
