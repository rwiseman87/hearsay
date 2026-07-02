//! Cross-platform streaming ASR via sherpa-onnx (streaming zipformer transducer). The "light live
//! model" of the Windows-floor design: live captions stream in on-device; speaker attribution is
//! deferred to the offline refine at stop. CPU by default (portable); accel is a sherpa build flag.
//!
//! This slice exposes a batch [`transcribe`](StreamingAsr::transcribe) (for WER verification); the
//! partial/final endpointed session that feeds the orchestrator's `Transcriber` is the next slice.

use std::path::Path;

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineTransducerModelConfig,
};

use crate::error::InferenceError;

/// Contract-fixed track sample rate (Hz).
const SAMPLE_RATE: i32 = 16_000;

/// The four files of a streaming zipformer transducer model.
#[derive(Debug, Clone, Copy)]
pub struct StreamingModel<'a> {
    pub encoder: &'a Path,
    pub decoder: &'a Path,
    pub joiner: &'a Path,
    pub tokens: &'a Path,
}

/// A streaming zipformer transducer recognizer (loaded once, reused per stream).
pub struct StreamingAsr {
    recognizer: OnlineRecognizer,
}

impl StreamingAsr {
    /// Load a streaming zipformer transducer. Endpoint detection is on (so a live session can split
    /// utterances); greedy decoding for speed on the low-end floor.
    pub fn load(model: StreamingModel) -> Result<Self, InferenceError> {
        let path = |p: &Path| Some(p.to_string_lossy().into_owned());
        let config = OnlineRecognizerConfig {
            model_config: OnlineModelConfig {
                transducer: OnlineTransducerModelConfig {
                    encoder: path(model.encoder),
                    decoder: path(model.decoder),
                    joiner: path(model.joiner),
                },
                tokens: path(model.tokens),
                ..Default::default()
            },
            decoding_method: Some("greedy_search".into()),
            enable_endpoint: true,
            rule1_min_trailing_silence: 2.4,
            rule2_min_trailing_silence: 1.2,
            rule3_min_utterance_length: 300.0,
            ..Default::default()
        };
        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            InferenceError::Streaming("failed to create streaming recognizer".into())
        })?;
        Ok(Self { recognizer })
    }

    /// Transcribe a whole 16 kHz mono buffer in one shot (drives the streaming recognizer to
    /// completion). For accuracy/WER verification; the live path feeds chunks incrementally.
    pub fn transcribe(&self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE, samples);
        stream.input_finished();
        while self.recognizer.is_ready(&stream) {
            self.recognizer.decode(&stream);
        }
        self.recognizer
            .get_result(&stream)
            .map(|r| r.text)
            .unwrap_or_default()
    }
}
