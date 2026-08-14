//! Case + punctuation restoration for the streaming ASR's output (sherpa-onnx online punctuation).
//!
//! The streaming zipformer emits bare uppercase tokens — `WELL I DON'T WISH TO SEE IT ANY MORE` —
//! because its training transcripts carry no case or punctuation. macOS does not have this problem:
//! Parakeet restores both natively. This closes that gap so a Windows transcript reads like a macOS
//! one, which also matters downstream, where the notes LLM summarizes the transcript text.
//!
//! The model only fires on lowercase input (fed the uppercase text verbatim it returns it
//! unchanged), so [`Punctuator::restore`] lowercases first and lets the model re-introduce case.

use std::path::Path;
use std::sync::Arc;

use sherpa_onnx::{OnlinePunctuation, OnlinePunctuationConfig, OnlinePunctuationModelConfig};

use crate::error::InferenceError;

/// The two files of an online punctuation model (`cnn_bilstm` + its BPE vocab).
#[derive(Debug, Clone, Copy)]
pub struct PunctuationModel<'a> {
    pub model: &'a Path,
    pub vocab: &'a Path,
}

/// A loaded punctuation model. Cheap to clone — clones share the model, and it is `Sync`, so both
/// stream workers can restore concurrently.
#[derive(Clone)]
pub struct Punctuator {
    inner: Arc<OnlinePunctuation>,
}

impl Punctuator {
    pub fn load(model: PunctuationModel) -> Result<Self, InferenceError> {
        let path = |p: &Path| Some(p.to_string_lossy().into_owned());
        let config = OnlinePunctuationConfig {
            model: OnlinePunctuationModelConfig {
                cnn_bilstm: path(model.model),
                bpe_vocab: path(model.vocab),
                ..Default::default()
            },
        };
        let inner = OnlinePunctuation::create(&config).ok_or_else(|| {
            InferenceError::Streaming("failed to create the punctuation model".into())
        })?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Restore case + punctuation. Returns the input unchanged when the model fails, so a
    /// punctuation problem can never cost the transcript its words.
    pub fn restore(&self, text: &str) -> String {
        if text.trim().is_empty() {
            return text.to_string();
        }
        match self.inner.add_punctuation(&text.to_lowercase()) {
            Some(out) if !out.trim().is_empty() => out,
            // The model returned nothing usable (or the text held an interior NUL): keep the words.
            _ => text.to_string(),
        }
    }
}
