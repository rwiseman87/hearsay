//! Cross-platform streaming ASR via sherpa-onnx (streaming zipformer transducer). The "light live
//! model" of the Windows-floor design: live captions stream in on-device; speaker attribution is
//! deferred to the offline refine at stop. CPU by default (portable); accel is a sherpa build flag.
//!
//! [`StreamingAsr::transcribe`] is the one-shot form (WER verification); [`StreamingAsr::session`]
//! returns a [`StreamingSession`] that emits growing partials + endpointed finals as PCM is fed —
//! the form the orchestrator's live `Transcriber` wraps (mapping [`StreamEvent`] -> its segment
//! type). The session is `Send` (the recognizer is held behind an `Arc`), so it drives from async.

use std::path::Path;
use std::sync::Arc;

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
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

/// Whether a [`StreamEvent`] is a growing in-progress hypothesis or a finalized utterance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEventKind {
    Partial,
    Final,
}

/// One live transcript event over `[start_s, end_s)` (stream-relative seconds; the orchestrator
/// shifts these onto the meeting clock).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEvent {
    pub kind: StreamEventKind,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// A streaming zipformer transducer recognizer (loaded once; drive one [`StreamingSession`] per
/// stream). Cheap to clone — clones share the loaded recognizer.
#[derive(Clone)]
pub struct StreamingAsr {
    recognizer: Arc<OnlineRecognizer>,
}

impl StreamingAsr {
    /// Load a streaming zipformer transducer. Endpoint detection is on (so a session splits
    /// utterances on trailing silence); greedy decoding for speed on the low-end floor.
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
        Ok(Self {
            recognizer: Arc::new(recognizer),
        })
    }

    /// Transcribe a whole 16 kHz mono buffer in one shot (drives the streaming recognizer to
    /// completion). For accuracy/WER verification; the live path uses [`session`](Self::session).
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

    /// Begin a live session: feed PCM chunks and receive partial/final [`StreamEvent`]s.
    pub fn session(&self) -> StreamingSession {
        StreamingSession {
            recognizer: self.recognizer.clone(),
            stream: self.recognizer.create_stream(),
            fed_samples: 0,
            utterance_start_s: 0.0,
            last_partial: String::new(),
        }
    }
}

/// A live streaming-ASR session over one audio stream. Feed 16 kHz mono PCM chunks; each
/// [`feed`](Self::feed) returns any events produced (a changed partial, or a final when the
/// endpointer fires on trailing silence). Call [`finish`](Self::finish) at end-of-input to flush
/// the trailing utterance.
pub struct StreamingSession {
    recognizer: Arc<OnlineRecognizer>,
    stream: OnlineStream,
    fed_samples: usize,
    utterance_start_s: f64,
    last_partial: String,
}

impl StreamingSession {
    fn now_s(&self) -> f64 {
        self.fed_samples as f64 / f64::from(SAMPLE_RATE)
    }

    fn current_text(&self) -> String {
        self.recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    /// Feed one chunk of PCM. Returns a `Final` when the endpointer fires (then resets for the next
    /// utterance), otherwise a `Partial` when the in-progress hypothesis changed.
    pub fn feed(&mut self, samples: &[f32]) -> Vec<StreamEvent> {
        self.stream.accept_waveform(SAMPLE_RATE, samples);
        self.fed_samples += samples.len();
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let text = self.current_text();
        let end_s = self.now_s();

        if self.recognizer.is_endpoint(&self.stream) {
            let mut events = Vec::new();
            if !text.is_empty() {
                events.push(StreamEvent {
                    kind: StreamEventKind::Final,
                    text,
                    start_s: self.utterance_start_s,
                    end_s,
                });
            }
            self.recognizer.reset(&self.stream);
            self.utterance_start_s = end_s;
            self.last_partial.clear();
            events
        } else if !text.is_empty() && text != self.last_partial {
            self.last_partial = text.clone();
            vec![StreamEvent {
                kind: StreamEventKind::Partial,
                text,
                start_s: self.utterance_start_s,
                end_s,
            }]
        } else {
            Vec::new()
        }
    }

    /// Signal end-of-input and flush the trailing utterance as a `Final` (if any text remains).
    pub fn finish(self) -> Vec<StreamEvent> {
        self.stream.input_finished();
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let text = self.current_text();
        if text.is_empty() {
            Vec::new()
        } else {
            vec![StreamEvent {
                kind: StreamEventKind::Final,
                text,
                start_s: self.utterance_start_s,
                end_s: self.now_s(),
            }]
        }
    }
}
