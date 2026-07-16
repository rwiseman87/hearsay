//! Platform backend selection for Hearsay: the concrete capture + inference stack assembled behind
//! the [`hearsay_engine::LiveEngine`] seam, kept out of the web-API crate.
//!
//! [`build_engine`] constructs the macOS backend (the Swift `hearsay-helper` capture + the
//! FluidAudio live sidecars + the whisper offline refine) inside an [`Orchestrator`] and returns it
//! as an `Arc<dyn LiveEngine>` — all `hearsay-core`'s binary needs. This is the one place a future
//! `WindowsBackend` is wired in, so the HTTP crate keeps depending only on the neutral seam.
//!
//! `SherpaTranscriber` (behind the `sherpa` feature) is the pure-Rust live `Transcriber` for the
//! (unbuilt) Windows path; it lives here because it needs both `hearsay-orchestrator` (the trait) and
//! `hearsay-inference` (the ASR), and is feature-gated so the macOS bundle never compiles onnxruntime.
//! [`probe_permissions`] is re-exported so the API's Permissions panel reaches the capture helper
//! without a direct `hearsay-capture` dependency.

mod mac;
pub mod reconcile;
#[cfg(feature = "sherpa")]
mod streaming_transcriber;

pub use hearsay_capture::{probe_permissions, PermissionsSnapshot};
pub use mac::build_engine;
#[cfg(feature = "sherpa")]
pub use streaming_transcriber::SherpaTranscriber;
