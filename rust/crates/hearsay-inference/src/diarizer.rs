//! The diarizer seam: turn a Them track into speaker turns + per-speaker voiceprints, independent of
//! the backend. The macOS path drives the Swift `hearsay-diarize` FluidAudio sidecar
//! ([`SwiftDiarizer`](crate::SwiftDiarizer), in `refine`); the cross-platform path drives sherpa-onnx
//! (`SherpaDiarizer`, `sherpa` feature). The offline refine consumes a `&dyn Diarizer`, so either
//! plugs in without touching the refine.

use std::collections::HashMap;

use crate::error::InferenceError;

/// One diarizer turn: a 1-based speaker ordinal (by first appearance) over `[start_s, end_s)`,
/// track-relative seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct DiarTurn {
    pub speaker: i64,
    pub start_s: f64,
    pub end_s: f64,
}

/// A diarization result: ordinal speaker turns (start-sorted) + each speaker's raw mean voiceprint by
/// ordinal (the refine L2-normalizes these into stored centroids). `embeddings` is empty when the
/// diarizer emits none. Mirrors the Swift `hearsay-diarize` sidecar's `turns` + `speakers` shape.
#[derive(Debug, Clone, Default)]
pub struct Diarization {
    pub turns: Vec<DiarTurn>,
    pub embeddings: HashMap<i64, Vec<f32>>,
}

/// Re-diarize a 16 kHz mono Them track into ordinal turns + per-speaker voiceprints. The offline
/// refine drives one of these (the Swift [`SwiftDiarizer`](crate::SwiftDiarizer) sidecar on macOS, or
/// `SherpaDiarizer` on the cross-platform path) instead of hard-spawning a subprocess, so the Windows
/// diarizer can plug in.
pub trait Diarizer {
    /// Diarize `them_samples` (16 kHz mono). [`InferenceError::NoSpeech`] signals a benign
    /// no-remote-speech track the refine treats as a no-op rather than a failure.
    fn diarize(&self, them_samples: &[f32]) -> Result<Diarization, InferenceError>;
}
