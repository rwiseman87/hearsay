//! Speaker attribution — pure logic, unit-tested in isolation.
//!
//! Cluster -> name binding by weighted-majority vote over sparse hints (a single wrong hint must
//! never flip a stable binding); cross-meeting voiceprint cosine matching; manual labels lock a
//! binding (votes cannot override).
//!
//! Rust port of `src/hearsay/services/speakers.py` and the diarization mapping helpers.
//!
//! Scaffold: see `docs/architecture-cross-platform.md`.
