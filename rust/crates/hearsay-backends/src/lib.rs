//! The macOS backend for Hearsay: the concrete capture + inference stack assembled behind
//! the [`hearsay_engine::LiveEngine`] seam, kept out of the web-API crate.
//!
//! `build_engine` takes an `EngineConfig` and returns an `Arc<dyn LiveEngine>` — all
//! `hearsay-core`'s binary needs. It wires the Swift `hearsay-helper` capture + the FluidAudio
//! live sidecars + the offline refine. The HTTP crate keeps depending only on the neutral
//! seam. [`probe_permissions`] is re-exported so the API's Permissions panel reaches the prober
//! without a direct `hearsay-capture` dependency.

use std::path::PathBuf;
use std::time::Duration;

use sqlx::SqlitePool;

use hearsay_orchestrator::{Defaults, RefineCoverage, RefineGap, RefineResult, RefinedThemSegment};

pub mod archive;
mod mac;
pub mod reconcile;
#[cfg(feature = "scripted")]
mod scripted;
mod summarizer;

pub use hearsay_capture::{probe_permissions, PermissionsSnapshot};
// The default notes prompt lives in the dependency-free `hearsay-notes-prompt` crate (shared with the
// notes sidecar); re-export it so `hearsay-core` can seed the config default without a direct dep.
pub use hearsay_notes_prompt::DEFAULT_NOTES_PROMPT;
pub use mac::build_engine;
#[cfg(feature = "scripted")]
pub use scripted::build_scripted_engine;

/// Everything `build_engine` needs, resolved by `hearsay-core` from its `Settings` plus the CLI.
pub struct EngineConfig {
    pub pool: SqlitePool,
    /// Root of the per-meeting output folders.
    pub output_dir: PathBuf,
    /// The Swift `hearsay-helper` capture binary (the `-live`/`-me`/`-diarize` sidecars
    /// resolve as siblings).
    pub helper_path: PathBuf,
    /// Run capture with generated audio (`--synthetic`): no devices touched, no permission prompts.
    pub synthetic: bool,
    /// Pre-warm the transcription sidecars at boot. False until the models are on disk; setup
    /// releases it with `LiveEngine::start_prewarm`.
    pub prewarm: bool,
    /// Deadline for the refine sidecar run (diarize + transcribe).
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
    /// The `hearsay-notes` sidecar binary (a sibling of the core) that runs the local-LLM notes step
    /// out-of-process, so llama.cpp never links into the core.
    pub notes_binary: PathBuf,
}

/// Map the inference crate's refine output onto the orchestrator's `RefineResult`.
pub(crate) fn map_refine_output(output: hearsay_inference::RefineOutput) -> RefineResult {
    RefineResult {
        segments: output
            .segments
            .into_iter()
            .map(|s| RefinedThemSegment {
                ordinal: s.ordinal,
                text: s.text,
                start_s: s.start_s,
                end_s: s.end_s,
            })
            .collect(),
        centroids: output.centroids,
        coverage: output.coverage.map(|c| RefineCoverage {
            fraction: c.fraction(),
            gaps: c
                .uncovered
                .iter()
                .map(|r| RefineGap {
                    start_s: r.start,
                    end_s: r.end,
                })
                .collect(),
        }),
    }
}

impl EngineConfig {
    /// The orchestrator defaults this config carries.
    pub(crate) fn defaults(&self) -> Defaults {
        Defaults {
            record: self.record,
            auto_refine: self.auto_refine,
            recognition_threshold: self.recognition_threshold,
            inactivity_prompt: self.inactivity_prompt,
            inactivity_auto_end: self.inactivity_auto_end,
            inactivity_prompt_minutes: self.inactivity_prompt_minutes,
            inactivity_end_minutes: self.inactivity_end_minutes,
            notes_enabled: self.notes_enabled,
            notes_model: self.notes_model.clone(),
        }
    }
}
