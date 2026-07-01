//! Orchestration: spawns and supervises the capture and inference sidecars, routes 16 kHz PCM
//! (Me/Them) between them, and owns the live transcription pipeline state machine (partials/finals,
//! offline refine at stop). Uses `tokio::process`; talks the `hearsay-ipc` contract.
//!
//! Rust port of `src/hearsay/helper/supervisor.py` and `src/hearsay/transcript/`.
//!
//! Scaffold: see `docs/architecture-cross-platform.md`.
