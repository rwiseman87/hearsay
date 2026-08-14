//! Speaker attribution — pure logic (no ML, no I/O), unit-tested in isolation.
//!
//! Cross-meeting voiceprint matching, the over-split consolidation pass `consolidate_speakers`, and
//! the diarization mapping helpers `order_speakers` / `assign_segment_speaker`.
//! See `docs/architecture.md`.

pub mod consolidate;
pub mod eval;
pub mod mapping;
pub mod voiceprint;

pub use consolidate::{consolidate_speakers, ConsolidateConfig, Consolidation};
pub use eval::{count_error, der, speaker_count, DerBreakdown};
pub use mapping::{
    assign_segment_speaker, max_overlap_turn, order_first_appearance, order_speakers, SpeakerTurn,
};
pub use voiceprint::{
    best_identity, centroid_from_bytes, centroid_to_bytes, cosine, l2_normalize, match_identity,
};
