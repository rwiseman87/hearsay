//! Local-only inference for Hearsay: the offline whisper ASR/refine.
//!
//! [`WhisperAsr`] loads a GGML whisper model and transcribes 16 kHz mono audio into timestamped
//! segments (whisper.cpp via `whisper-rs`; CPU by default, Metal GPU accel is the `metal` Cargo
//! feature); it backs the orchestrator's post-meeting refine.

mod asr;
mod audio;
mod diarizer;
mod error;
mod refine;

pub use asr::{AsrSegment, Coverage, Transcription, WhisperAsr, DEFAULT_LANGUAGE, LOOP_MIN_CYCLES};
pub use audio::{read_them_channel, read_wav_mono_16k, SAMPLE_RATE};
pub use diarizer::{DiarTurn, Diarization, Diarizer};
pub use error::InferenceError;
pub use refine::{
    refine_audio_file, refine_audio_file_with, refine_them, refine_them_with, RefineOutput,
    RefinedSegment, SwiftDiarizer,
};
