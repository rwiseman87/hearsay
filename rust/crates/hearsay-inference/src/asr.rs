//! Offline ASR via whisper.cpp (`whisper-rs`). Loads a GGML model once and transcribes 16 kHz mono
//! PCM into timestamped segments. This is the offline path — the accuracy-verification harness and
//! the orchestrator's post-meeting refine. GPU acceleration (Metal / Vulkan / CUDA) is a
//! `whisper-rs` Cargo feature; with none enabled it runs on CPU.

use std::path::Path;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::error::InferenceError;

/// One transcribed segment. Times are seconds from the start of the given audio.
#[derive(Debug, Clone, PartialEq)]
pub struct AsrSegment {
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// A loaded whisper.cpp model. Cheap to clone the handle; `transcribe` creates a fresh state per
/// call so it is safe to reuse across audio.
pub struct WhisperAsr {
    ctx: WhisperContext,
}

impl WhisperAsr {
    /// Load a GGML whisper model (e.g. `ggml-base.bin`, `ggml-large-v3-turbo.bin`).
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self, InferenceError> {
        let ctx = WhisperContext::new_with_params(
            model_path.as_ref(),
            WhisperContextParameters::default(),
        )
        .map_err(|e| InferenceError::Whisper(format!("load model: {e}")))?;
        Ok(WhisperAsr { ctx })
    }

    /// Transcribe 16 kHz mono `samples` (float in [-1, 1]) into timestamped segments (English,
    /// greedy). Timestamps come from whisper's centisecond segment bounds.
    pub fn transcribe(&self, samples: &[f32]) -> Result<Vec<AsrSegment>, InferenceError> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| InferenceError::Whisper(format!("create state: {e}")))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, samples)
            .map_err(|e| InferenceError::Whisper(format!("transcribe: {e}")))?;

        let n = state.full_n_segments();
        let mut segments = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n {
            let Some(segment) = state.get_segment(i) else {
                continue;
            };
            let text = segment
                .to_str_lossy()
                .map_err(|e| InferenceError::Whisper(format!("segment text: {e}")))?
                .trim()
                .to_string();
            segments.push(AsrSegment {
                text,
                start_s: segment.start_timestamp() as f64 / 100.0,
                end_s: segment.end_timestamp() as f64 / 100.0,
            });
        }
        Ok(segments)
    }
}
