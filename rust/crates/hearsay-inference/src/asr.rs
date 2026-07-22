//! Offline ASR via whisper.cpp (`whisper-rs`). Loads a GGML model once and transcribes 16 kHz mono
//! PCM into timestamped segments. This is the offline path — the accuracy-verification harness and
//! the orchestrator's post-meeting refine. GPU acceleration (Metal / Vulkan / CUDA) is a
//! `whisper-rs` Cargo feature; with none enabled it runs on CPU.

use std::path::Path;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::error::InferenceError;

/// Default whisper transcription language (a whisper language code). English by default; a caller
/// overrides it via [`WhisperAsr::with_language`] (the offline refine keeps this default).
pub const DEFAULT_LANGUAGE: &str = "en";

/// Anti-loop entropy threshold (whisper.cpp `entropy_thold`; its default is 2.4).
///
/// Long conversational audio can drop the greedy decoder into a self-sustaining repetition loop —
/// one confident phrase emitted over and over in 1-second segments, filling every remaining 30-s
/// window to the end of the track (a 17-minute meeting came back as one phrase repeated 473
/// times). The fallback gate that should catch this compares the entropy of the window's last 32
/// tokens against the threshold, and the loops clear the stock 2.4: measured on a real meeting, a
/// single-phrase loop scored 2.45 and a two-phrase ping-pong loop 2.83, while genuine speech
/// windows scored 3.06-3.38. 3.0 sits in that gap — every observed loop now fails the gate, which
/// retries the window at a higher temperature and breaks the attractor, and real windows pass
/// untouched. Verified loop-free on the failing meeting and regression-free on a known-good one.
pub const DEFAULT_ENTROPY_THOLD: f32 = 3.0;

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
    language: String,
    entropy_thold: f32,
}

impl WhisperAsr {
    /// Load a GGML whisper model (e.g. `ggml-base.bin`, `ggml-large-v3-turbo.bin`). Transcription
    /// language defaults to [`DEFAULT_LANGUAGE`]; override it with
    /// [`with_language`](Self::with_language).
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self, InferenceError> {
        let ctx = WhisperContext::new_with_params(
            model_path.as_ref(),
            WhisperContextParameters::default(),
        )
        .map_err(|e| InferenceError::Whisper(format!("load model: {e}")))?;
        Ok(WhisperAsr {
            ctx,
            language: DEFAULT_LANGUAGE.to_string(),
            entropy_thold: DEFAULT_ENTROPY_THOLD,
        })
    }

    /// Override the anti-loop entropy threshold ([`DEFAULT_ENTROPY_THOLD`]); the probe harness
    /// uses this to measure candidate thresholds against recorded meetings.
    pub fn with_entropy_thold(mut self, thold: f32) -> Self {
        self.entropy_thold = thold;
        self
    }

    /// Set the transcription language (a whisper language code, e.g. `"de"`, or `"auto"` to detect);
    /// defaults to [`DEFAULT_LANGUAGE`]. The offline refine keeps the default.
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    /// Transcribe 16 kHz mono `samples` (float in [-1, 1]) into timestamped segments (greedy; the
    /// model's `language`, default English). Timestamps come from whisper's centisecond segment
    /// bounds.
    pub fn transcribe(&self, samples: &[f32]) -> Result<Vec<AsrSegment>, InferenceError> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| InferenceError::Whisper(format!("create state: {e}")))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_entropy_thold(self.entropy_thold);
        params.set_language(Some(self.language.as_str()));
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
