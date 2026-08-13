//! Platform backend selection for Hearsay: the concrete capture + inference stack assembled behind
//! the [`hearsay_engine::LiveEngine`] seam, kept out of the web-API crate.
//!
//! Each platform module exports a `build_engine(EngineConfig) -> Arc<dyn LiveEngine>` — all
//! `hearsay-core`'s binary needs. macOS (`mac`) wires the Swift `hearsay-helper` capture + the
//! FluidAudio live sidecars + the whisper offline refine; Windows (`windows`) wires WASAPI capture +
//! the sherpa live/diarize path. The HTTP crate keeps depending only on the neutral seam.
//!
//! `SherpaTranscriber` (behind the `sherpa` feature) is the pure-Rust live `Transcriber` for the
//! Windows path; it lives here because it needs both `hearsay-orchestrator` (the trait) and
//! `hearsay-inference` (the ASR), and is feature-gated so the macOS bundle never compiles
//! onnxruntime. [`probe_permissions`] is re-exported so the API's Permissions panel reaches the
//! per-OS prober without a direct `hearsay-capture` dependency.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;

use hearsay_db::models::Stream;
use hearsay_engine::LiveEngine;
use hearsay_orchestrator::testing::{ProgressiveBackend, ProgressivePlan, ScriptedSummarizer};
use hearsay_orchestrator::{AudioChunk, CaptureChunk, Orchestrator, SegmentKind, SidecarSegment};

pub mod archive;
#[cfg(target_os = "macos")]
mod mac;
pub mod reconcile;
#[cfg(feature = "sherpa")]
mod streaming_transcriber;
mod summarizer;
#[cfg(all(target_os = "windows", feature = "sherpa"))]
mod windows;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("hearsay-backends supports macOS and Windows only");
#[cfg(all(target_os = "windows", not(feature = "sherpa")))]
compile_error!(
    "the Windows backend needs the sherpa live/diarize path: build with --features sherpa"
);

pub use hearsay_capture::{probe_permissions, LoopbackMode, PermissionsSnapshot};
// The default notes prompt lives in the dependency-free `hearsay-notes-prompt` crate (shared with the
// notes sidecar); re-export it so `hearsay-core` can seed the config default without a direct dep.
pub use hearsay_notes_prompt::DEFAULT_NOTES_PROMPT;
#[cfg(target_os = "macos")]
pub use mac::build_engine;
#[cfg(feature = "sherpa")]
pub use streaming_transcriber::SherpaTranscriber;
#[cfg(all(target_os = "windows", feature = "sherpa"))]
pub use windows::build_engine;

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
    /// The `hearsay-notes` sidecar binary (a sibling of the core) that runs the local-LLM notes step
    /// out-of-process, so llama.cpp never links into the core alongside whisper.
    pub notes_binary: PathBuf,
    /// Windows: directory holding the sherpa live/diarize models (streaming zipformer + pyannote
    /// segmentation + speaker embedding). Unused on macOS (FluidAudio models seed separately).
    pub sherpa_models_dir: PathBuf,
    /// Windows: which WASAPI loopback path captures Them. Unused on macOS.
    pub win_loopback_mode: LoopbackMode,
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
    Orchestrator::new(config.pool, config.output_dir, backend)
        .with_defaults(
            config.record,
            config.auto_refine,
            config.recognition_threshold,
            config.inactivity_prompt,
            config.inactivity_auto_end,
            config.inactivity_prompt_minutes,
            config.inactivity_end_minutes,
            config.notes_enabled,
            config.notes_model,
        )
        .with_summarizer(summarizer)
        .into_arc()
}

/// The canned conversation `HEARSAY_SCRIPTED` replays: two capture chunks to anchor the shared clock
/// (Me at t0, Them +0.5 s), then a short Me/Them exchange whose partials + finals emit ~0.4-1.2 s into
/// the meeting so the browser E2E can watch the transcript grow, then assert the two finalized turns.
fn scripted_meeting_plan() -> ProgressivePlan {
    fn seg(
        kind: SegmentKind,
        text: &str,
        start_s: f64,
        end_s: f64,
        speaker: Option<i64>,
    ) -> SidecarSegment {
        SidecarSegment {
            kind,
            text: text.to_string(),
            start_s,
            end_s,
            speaker,
        }
    }
    ProgressivePlan {
        chunks: vec![
            CaptureChunk {
                stream: Stream::Me,
                chunk: AudioChunk {
                    host_ts: 1_000_000_000,
                    samples: vec![0.05, 0.05],
                },
            },
            CaptureChunk {
                stream: Stream::Them,
                chunk: AudioChunk {
                    host_ts: 1_500_000_000,
                    samples: vec![0.05, 0.05, 0.05],
                },
            },
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
