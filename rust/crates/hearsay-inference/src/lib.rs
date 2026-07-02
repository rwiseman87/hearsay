//! Local-only inference for Hearsay.
//!
//! Being built Mac-first, smallest-verifiable-first (see `docs/TODO.md`): the offline ASR path is
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
mod error;
mod refine;

pub use asr::{AsrSegment, WhisperAsr};
pub use audio::{read_them_channel, read_wav_mono_16k, SAMPLE_RATE};
pub use error::InferenceError;
pub use refine::{refine_audio_file, refine_them, RefinedSegment};
