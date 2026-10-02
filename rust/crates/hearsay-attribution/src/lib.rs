//! Speaker attribution — pure logic (no ML, no I/O), unit-tested in isolation.
//!
//! Cross-meeting voiceprint matching, the diarization mapping helpers `order_speakers` /
//! `assign_segment_speaker`, diarization metrics (`eval`), and WER/cpWER/latency metrics (`wer`).
//! See `docs/architecture.md`.

pub mod eval;
pub mod mapping;
pub mod voiceprint;
pub mod wer;

pub use eval::{count_error, der, speaker_count, DerBreakdown};
pub use mapping::{assign_segment_speaker, order_speakers, SpeakerTurn};
pub use voiceprint::{
    best_identity, centroid_from_bytes, centroid_to_bytes, cosine, l2_normalize, match_identity,
};
pub use wer::{cpwer, normalize, percentiles, word_errors, Cpwer, Percentiles, WerBreakdown};
