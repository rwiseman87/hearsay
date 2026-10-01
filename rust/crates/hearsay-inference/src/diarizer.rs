//! The diarizer seam: turn a Them track into speaker turns + per-speaker voiceprints, independent of
//! the backend. The Swift `hearsay-diarize` FluidAudio sidecar ([`SwiftDiarizer`](crate::SwiftDiarizer),
//! in `refine`) is the implementation; the offline refine consumes a `&dyn Diarizer`, so tests plug
//! in a scripted one without touching the refine.

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
/// refine drives one of these (the Swift [`SwiftDiarizer`](crate::SwiftDiarizer) sidecar) instead of
/// hard-spawning a subprocess, so tests can plug in a scripted diarizer.
pub trait Diarizer {
    /// Diarize `them_samples` (16 kHz mono). [`InferenceError::NoSpeech`] signals a benign
    /// no-remote-speech track the refine treats as a no-op rather than a failure.
    fn diarize(&self, them_samples: &[f32]) -> Result<Diarization, InferenceError>;
}
