//! Speaker attribution — pure logic (no ML, no I/O), unit-tested in isolation.
//!
//! Rust port of the pure attribution logic in the Python core: cross-meeting voiceprint matching
//! (`hearsay.diarization.voiceprint`) and the diarization mapping helpers `order_speakers` /
//! `assign_segment_speaker` (`hearsay.diarization.offline`). See
//! `docs/architecture-cross-platform.md`.

pub mod mapping;
pub mod voiceprint;

pub use mapping::{assign_segment_speaker, order_speakers, SpeakerTurn};
pub use voiceprint::{centroid_from_bytes, centroid_to_bytes, cosine, match_identity};
