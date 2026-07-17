//! Speaker attribution — pure logic (no ML, no I/O), unit-tested in isolation.
//!
//! Cross-meeting voiceprint matching plus the diarization mapping helpers `order_speakers` /
//! `assign_segment_speaker`. See `docs/architecture.md`.

pub mod mapping;
pub mod voiceprint;

pub use mapping::{assign_segment_speaker, order_speakers, SpeakerTurn};
pub use voiceprint::{centroid_from_bytes, centroid_to_bytes, match_identity};
