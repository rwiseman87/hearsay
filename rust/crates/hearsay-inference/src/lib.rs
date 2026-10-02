//! Local-only inference for Hearsay: the offline refine, one `hearsay-diarize` FluidAudio sidecar
//! run that diarizes and transcribes the Them track, plus the attribution of each word to a speaker
//! and the coverage guard over the result. All the ML runs in the sidecar, on the Apple Neural
//! Engine; this crate owns the subprocess contract and the pure logic around it.

mod audio;
mod coverage;
mod error;
mod refine;

pub use audio::{read_them_channel, read_wav_mono_16k, SAMPLE_RATE};
pub use coverage::Coverage;
pub use error::InferenceError;
pub use refine::{
    diarize, refine_audio_file, refine_them, DiarTurn, Diarization, RefineOutput, RefinedSegment,
    ASR_MODEL,
};
