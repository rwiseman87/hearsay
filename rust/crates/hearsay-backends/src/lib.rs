//! Platform backend selection for Hearsay: the concrete capture + inference stack assembled behind
//! the [`hearsay_engine::LiveEngine`] seam, kept out of the web-API crate.
//!
//! `build_engine` takes an `EngineConfig` and returns an `Arc<dyn LiveEngine>` — all
//! `hearsay-core`'s binary needs. It wires the Swift `hearsay-helper` capture + the FluidAudio
//! live sidecars + the whisper offline refine. The HTTP crate keeps depending only on the neutral
//! seam. [`probe_permissions`] is re-exported so the API's Permissions panel reaches the prober
//! without a direct `hearsay-capture` dependency.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;

use hearsay_db::models::Stream;
use hearsay_engine::LiveEngine;
use hearsay_orchestrator::testing::{
    chunk, seg, ProgressiveBackend, ProgressivePlan, ScriptedSummarizer,
};
use hearsay_orchestrator::{
    Defaults, Orchestrator, RefineCoverage, RefineResult, RefinedThemSegment, SegmentKind,
};

pub mod archive;
#[cfg(target_os = "macos")]
mod mac;
pub mod reconcile;
mod summarizer;

pub use hearsay_capture::{probe_permissions, PermissionsSnapshot};
// The default notes prompt lives in the dependency-free `hearsay-notes-prompt` crate (shared with the
// notes sidecar); re-export it so `hearsay-core` can seed the config default without a direct dep.
pub use hearsay_notes_prompt::DEFAULT_NOTES_PROMPT;
#[cfg(target_os = "macos")]
pub use mac::build_engine;

/// Everything a platform `build_engine` needs, resolved by `hearsay-core` from its `Settings` plus
/// the CLI. One struct on every platform so the composition-root call site never forks; each
/// backend reads the fields that apply to it and ignores the rest.
pub struct EngineConfig {
    pub pool: SqlitePool,
    /// Root of the per-meeting output folders.
    pub output_dir: PathBuf,
    /// macOS: the Swift `hearsay-helper` capture binary (the `-live`/`-me`/`-diarize` sidecars
    /// resolve as siblings).
    pub helper_path: PathBuf,
    /// Run capture with generated audio (`--synthetic`): no devices touched, no permission prompts.
    pub synthetic: bool,
    /// Pre-warm the transcription sidecars at boot. False until the models are on disk; setup
    /// releases it with `LiveEngine::start_prewarm`.
    pub prewarm: bool,
    /// Config-default GGML whisper model for the offline refine (the Models panel overrides it).
    pub refine_model: PathBuf,
    /// Deadline for the refine's diarize step.
    pub refine_timeout: Duration,
    /// Whether the refine's whisper pass primes each 30-s window with the previous window's text.
    /// Off by default: measured on a 44-minute meeting it cost 2.4x the decode time and let one
    /// hallucinated silent window poison every window after it.
    pub refine_carry_over: bool,
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
    /// out-of-process, so llama.cpp never links into the core alongside whisper.
    pub notes_binary: PathBuf,
}

/// Map the inference crate's refine output onto the orchestrator's `RefineResult`. Identical on
/// every platform — only how the output is produced differs.
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
            recovered_spans: c.recovered.len(),
            unrecovered_spans: c.unrecovered.len(),
        }),
    }
}

impl EngineConfig {
    /// The orchestrator defaults this config carries. Every `build_engine` needs exactly this
    /// subset, so each platform lifts it the same way.
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

/// Assemble a deterministic, model-free [`LiveEngine`] that runs the real orchestrator pipeline +
/// persistence over a canned meeting — emitting its transcript progressively over the live WebSocket
/// during recording (via [`ProgressiveBackend`]) — instead of touching any capture device, ANE, or
/// GPU. Selected by the core's dev-only `HEARSAY_SCRIPTED` flag so the browser end-to-end test can
/// drive the real core *binary* with exact, assertable output. Platform-neutral: it reuses the same
/// `hearsay-orchestrator::testing` fakes the in-process Rust full-stack test does, so both paths
/// produce identical output. A canned summarizer backs the "Generate notes" step; no refiner is wired
/// (like the full-stack test), so stop just finalizes and keeps the live-emitted segments.
pub fn build_scripted_engine(config: EngineConfig) -> Arc<dyn LiveEngine> {
    let backend = Arc::new(ProgressiveBackend::new(scripted_meeting_plan()));
    let (summarizer, _) = ScriptedSummarizer::new(
        "Scripted summary for the end-to-end test.\n\n- Ship the browser E2E.",
    );
    let defaults = config.defaults();
    Orchestrator::new(config.pool, config.output_dir, backend)
        .with_defaults(defaults)
        .with_summarizer(summarizer)
        .into_arc()
}

/// The canned conversation `HEARSAY_SCRIPTED` replays: two capture chunks to anchor the shared clock
/// (Me at t0, Them +0.5 s), then a short Me/Them exchange whose partials + finals emit ~0.4-1.2 s into
/// the meeting so the browser E2E can watch the transcript grow, then assert the two finalized turns.
fn scripted_meeting_plan() -> ProgressivePlan {
    ProgressivePlan {
        chunks: vec![
            chunk(Stream::Me, 1_000_000_000, &[0.05, 0.05]),
            chunk(Stream::Them, 1_500_000_000, &[0.05, 0.05, 0.05]),
        ],
        me: vec![
            (
                Duration::from_millis(400),
                seg(SegmentKind::Partial, "hello", 0.0, 0.5, None),
            ),
            (
                Duration::from_millis(400),
                seg(SegmentKind::Final, "hello there", 0.0, 1.0, None),
            ),
        ],
        them: vec![
            (
                Duration::from_millis(600),
                seg(SegmentKind::Partial, "hi", 0.0, 0.5, None),
            ),
            (
                Duration::from_millis(600),
                seg(
                    SegmentKind::Final,
                    "hi everyone, thanks for joining",
                    1.0,
                    3.0,
                    Some(0),
                ),
            ),
        ],
    }
}
