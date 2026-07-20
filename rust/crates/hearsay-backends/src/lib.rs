//! Platform backend selection for Hearsay: the concrete capture + inference stack assembled behind
//! the [`hearsay_engine::LiveEngine`] seam, kept out of the web-API crate.
//!
//! Each platform module exports a `build_engine(EngineConfig) -> Arc<dyn LiveEngine>` — all
//! `hearsay-core`'s binary needs. macOS (`mac`) wires the Swift `hearsay-helper` capture + the
//! FluidAudio live sidecars + the whisper offline refine; Windows (`windows`, in progress — see
//! `docs/windows-port.md`) wires WASAPI capture + the sherpa live/diarize path. The HTTP crate
//! keeps depending only on the neutral seam.
//!
//! `SherpaTranscriber` (behind the `sherpa` feature) is the pure-Rust live `Transcriber` for the
//! Windows path; it lives here because it needs both `hearsay-orchestrator` (the trait) and
//! `hearsay-inference` (the ASR), and is feature-gated so the macOS bundle never compiles
//! onnxruntime. [`probe_permissions`] is re-exported so the API's Permissions panel reaches the
//! per-OS prober without a direct `hearsay-capture` dependency.

use std::path::PathBuf;
use std::time::Duration;

use sqlx::SqlitePool;

#[cfg(target_os = "macos")]
mod mac;
pub mod reconcile;
#[cfg(feature = "sherpa")]
mod streaming_transcriber;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("hearsay-backends supports macOS and Windows only");

pub use hearsay_capture::{probe_permissions, LoopbackMode, PermissionsSnapshot};
// The default notes prompt lives with the prompt builder in `hearsay-inference`; re-export it so
// `hearsay-core` (which reaches inference only through this crate) can seed the config default.
pub use hearsay_inference::DEFAULT_NOTES_PROMPT;
#[cfg(target_os = "macos")]
pub use mac::build_engine;
#[cfg(feature = "sherpa")]
pub use streaming_transcriber::SherpaTranscriber;

/// Everything a platform `build_engine` needs, resolved by `hearsay-core` from its `Settings` plus
/// the CLI. One struct on every platform so the composition-root call site never forks; each
/// backend reads the fields that apply to it and ignores the rest.
pub struct EngineConfig {
    pub pool: SqlitePool,
    /// Root of the per-meeting output folders.
    pub output_dir: PathBuf,
    /// macOS: the Swift `hearsay-helper` capture binary (the `-live`/`-me`/`-diarize` sidecars
    /// resolve as siblings). Unused on Windows (capture is in-process).
    pub helper_path: PathBuf,
    /// Run capture with generated audio (`--synthetic`): no devices touched, no permission prompts.
    pub synthetic: bool,
    /// Config-default GGML whisper model for the offline refine (the Models panel overrides it).
    pub refine_model: PathBuf,
    /// Deadline for the refine's diarize step.
    pub refine_timeout: Duration,
    pub record: bool,
    pub auto_refine: bool,
    pub recognition_threshold: f64,
    pub inactivity_prompt: bool,
    pub inactivity_auto_end: bool,
    pub inactivity_prompt_minutes: u64,
    pub inactivity_end_minutes: u64,
    pub notes_enabled: bool,
    /// Config-default GGUF notes model (the Models panel overrides it).
    pub notes_model: PathBuf,
    /// Config-default notes prompt template (the Models panel overrides it).
    pub notes_prompt: String,
    /// Windows: directory holding the sherpa live/diarize models (streaming zipformer + pyannote
    /// segmentation + speaker embedding). Unused on macOS (FluidAudio models seed separately).
    pub sherpa_models_dir: PathBuf,
    /// Windows: which WASAPI loopback path captures Them. Unused on macOS.
    pub win_loopback_mode: LoopbackMode,
}
