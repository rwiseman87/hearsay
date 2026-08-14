//! The macOS live-capture + offline-refine backend, assembled into an [`Orchestrator`] by
//! [`build_engine`]. It lives here, not in the `hearsay-core` binary, so the web-API crate does not
//! link the concrete capture/inference stack; the `LiveEngine` seam is honored at the composition root.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use sqlx::SqlitePool;

use hearsay_capture::SwiftHelperSource;
use hearsay_engine::LiveEngine;
use hearsay_orchestrator::{
    Backend, BackendInstance, Orchestrator, OrchestratorError, ProcessTranscriber, RefineResult,
    Refiner,
};

use crate::summarizer::SubprocessSummarizer;
use crate::EngineConfig;

/// Keeps one `hearsay-me` + `hearsay-live` pair pre-spawned so its FluidAudio/CoreML models load
/// (and the ANE warms) *before* the user hits start, off the meeting-start path. The pool holds the
/// spawned pair the moment it is spawned — while the models are still loading in the background —
/// so a meeting always *adopts* this pair (loaded or still loading) rather than cold-spawning a
/// second pair that would race it on the ANE and slow both. One pair is spawned at launch and a
/// replacement after each meeting adopts one (each process still serves exactly one meeting — no
/// cross-meeting state is reused). The capture source is deliberately *not* pooled: spawning it
/// touches the mic/tap and can raise a TCC prompt, so it must stay fresh per meeting.
struct SidecarPool {
    me_binary: PathBuf,
    them_binary: PathBuf,
    /// The spawned pair the next meeting adopts (its models loading in the background, or already
    /// loaded). `None` only before the first spawn, after a meeting takes it (until the replacement
    /// spawns), or if a spawn failed (the next meeting then cold-starts).
    warm: Mutex<Option<(ProcessTranscriber, ProcessTranscriber)>>,
}

impl SidecarPool {
    fn new(helper_path: &Path) -> Self {
        SidecarPool {
            me_binary: helper_path.with_file_name("hearsay-me"),
            them_binary: helper_path.with_file_name("hearsay-live"),
            warm: Mutex::new(None),
        }
    }

    /// Take the spawned pair if one is present (leaving the pool empty until the next replenish).
    /// `None` means the caller must cold-spawn — no prewarm pair was available.
    fn take(&self) -> Option<(ProcessTranscriber, ProcessTranscriber)> {
        self.warm.lock().unwrap().take()
    }

    /// Whether a warm pair is present *and* both sidecars have finished loading their models, so the
    /// next meeting would start transcribing immediately. Answers the API's "Start" gate. `false`
    /// while the pair is still loading, or in the brief window after a meeting takes the pair and
    /// before its replacement has spawned.
    fn is_ready(&self) -> bool {
        self.warm
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(me, them)| me.is_ready() && them.is_ready())
    }

    /// Spawn a replacement pair into the pool if none is present. The two sidecar processes begin
    /// loading their models immediately, in the background; the spawn itself returns at once, so this
    /// is cheap to call. Idempotent; call at startup and after each [`take`](Self::take). A spawn
    /// failure (a missing/broken sidecar binary) is logged and leaves the pool empty, so the next
    /// meeting simply cold-starts. Must be called from within the Tokio runtime (the sidecar child
    /// registers with the reactor).
    fn ensure_warm(&self) {
        let mut guard = self.warm.lock().unwrap();
        if let Some((me, them)) = guard.as_mut() {
            if me.is_alive() && them.is_alive() {
                return; // a healthy pair is present (loaded, or still loading)
            }
            // A pooled sidecar exited (e.g. a warm whose model load failed) — drop the dead pair
            // (kill_on_drop reaps the survivor) and re-spawn below, so a dead pair never wedges the
            // "Start" gate: is_ready() would otherwise report false for it forever.
            tracing::warn!("prewarmed sidecar pair died before use; re-warming");
            *guard = None;
        }
        let mut me = ProcessTranscriber::new(self.me_binary.clone());
        let mut them = ProcessTranscriber::new(self.them_binary.clone());
        // Spawn both; each process loads its own models on the ANE concurrently and signals ready
        // when done. spawn_warming returns immediately — the load runs in the background.
        match (me.spawn_warming(), them.spawn_warming()) {
            (Ok(()), Ok(())) => {
                *guard = Some((me, them));
                tracing::info!("prewarming live sidecars (models loading in the background)");
            }
            (me_res, them_res) => {
                // Drop whichever spawned (kill_on_drop reaps it); the next meeting cold-starts.
                if let Err(err) = me_res {
                    tracing::warn!(error = %err, "prewarm spawn of hearsay-me failed; next meeting cold-starts");
                }
                if let Err(err) = them_res {
                    tracing::warn!(error = %err, "prewarm spawn of hearsay-live failed; next meeting cold-starts");
                }
            }
        }
    }
}

/// The macOS live-capture backend: the Swift `hearsay-helper` for capture + the built `hearsay-live`
/// (Them: diarization + ASR) and `hearsay-me` (Me: VAD + ASR) FluidAudio sidecars. The transcriber
/// sidecars are drawn from a [`SidecarPool`] (pre-warmed off the start path) when hot, else spawned
/// cold; the capture source is always fresh. Reuses the proven Swift stack behind the traits.
struct MacBackend {
    helper_path: PathBuf,
    synthetic: bool,
    pool: SidecarPool,
}

impl MacBackend {
    fn new(helper_path: PathBuf, synthetic: bool) -> Self {
        let pool = SidecarPool::new(&helper_path);
        MacBackend {
            helper_path,
            synthetic,
            pool,
        }
    }

    /// Spawn the first sidecar pair now (at launch) so its models start loading before the first
    /// meeting, off the start path. Safe to call once before serving; a spawn failure just leaves
    /// the first meeting to cold-start.
    fn prewarm(&self) {
        self.pool.ensure_warm();
    }
}

impl Backend for MacBackend {
    fn build(&self) -> BackendInstance {
        let (me, them) = match self.pool.take() {
            Some(pair) => {
                tracing::info!("adopting prewarmed live sidecars");
                pair
            }
            None => (
                ProcessTranscriber::new(self.helper_path.with_file_name("hearsay-me")),
                ProcessTranscriber::new(self.helper_path.with_file_name("hearsay-live")),
            ),
        };
        // Deliberately do NOT warm the replacement here: the meeting's own sidecars are about to run
        // on the ANE, and a concurrent warm load starves (and can fail) against them. The pool is
        // re-warmed on `meeting_ended` instead, once this meeting's sidecars have been torn down.
        BackendInstance {
            source: Box::new(
                SwiftHelperSource::new(self.helper_path.clone()).synthetic(self.synthetic),
            ),
            me: Box::new(me),
            them: Box::new(them),
        }
    }

    fn sidecars_ready(&self) -> bool {
        self.pool.is_ready()
    }

    fn ensure_pool_warm(&self) {
        self.pool.ensure_warm();
    }
}

/// The post-meeting offline refine backend: re-diarize the Them track with the Swift
/// `hearsay-diarize` FluidAudio sidecar + re-transcribe with whisper (`hearsay-inference`). Wired
/// into the orchestrator so a meeting auto-refines at stop — the same refiner the manual
/// `/rediarize` route now drives through [`LiveEngine::rediarize`].
struct MacRefiner {
    pool: SqlitePool,
    diarize_path: PathBuf,
    /// Bundled config default; the effective model is the `models` preference override else this,
    /// resolved from the DB at each refine so a Models-panel change applies with no restart.
    default_model: PathBuf,
    timeout: Duration,
}

#[async_trait]
impl Refiner for MacRefiner {
    async fn refine(&self, audio_path: &Path) -> Result<RefineResult, OrchestratorError> {
        if !self.diarize_path.exists() {
            return Err(OrchestratorError::Backend(format!(
                "hearsay-diarize sidecar not found at {} (build it with `make swift-build`)",
                self.diarize_path.display()
            )));
        }
        let model = hearsay_db::queries::effective_refine_model(&self.pool, &self.default_model)
            .await
            .map_err(|e| OrchestratorError::Backend(format!("resolve refine model: {e}")))?;
        let audio = audio_path.to_path_buf();
        let diarize = self.diarize_path.clone();
        let timeout = self.timeout;
        // whisper + the diarize subprocess are blocking — run off the async runtime.
        let output = tokio::task::spawn_blocking(move || {
            hearsay_inference::refine_audio_file(&audio, &diarize, &model, timeout)
        })
        .await
        .map_err(|e| OrchestratorError::Backend(format!("refine task panicked: {e}")))?;
        let output = match output {
            Ok(output) => output,
            // A silent / Me-only meeting has no remote speech to refine. That is benign, not a
            // failure: return an empty result so both the auto-refine and the manual rediarize treat
            // it as a no-op and keep the existing segments (`replace_them_segments` skips an empty
            // result rather than wiping the transcript).
            Err(hearsay_inference::InferenceError::NoSpeech) => return Ok(RefineResult::default()),
            Err(e) => return Err(OrchestratorError::Backend(format!("refine failed: {e}"))),
        };
        Ok(crate::map_refine_output(output))
    }
}

/// Assemble the macOS live engine: the Swift capture helper + FluidAudio live sidecars ([`MacBackend`])
/// and the whisper offline refine ([`MacRefiner`]) inside an [`Orchestrator`], returned as the neutral
/// [`LiveEngine`] the HTTP crate consumes. Prewarms the first sidecar pair and installs the
/// orchestrator's weak self-reference (so a capture death finalizes the meeting) before returning.
/// Takes the platform-neutral [`EngineConfig`] rather than `hearsay_core::Settings` so this crate
/// does not depend on the web crate. Call from within the Tokio runtime (prewarm spawns sidecar
/// children).
pub fn build_engine(config: EngineConfig) -> Arc<dyn LiveEngine> {
    let defaults = config.defaults();
    let EngineConfig {
        pool,
        output_dir,
        helper_path,
        synthetic,
        prewarm,
        refine_model,
        refine_timeout,
        // The editable-settings defaults are lifted whole by `config.defaults()` above.
        record: _,
        auto_refine: _,
        recognition_threshold: _,
        inactivity_prompt: _,
        inactivity_auto_end: _,
        inactivity_prompt_minutes: _,
        inactivity_end_minutes: _,
        notes_enabled: _,
        notes_model,
        notes_prompt,
        notes_binary,
        // Windows-only fields; the mac backend has no use for them.
        sherpa_models_dir: _,
        win_loopback_mode: _,
    } = config;
    let backend = Arc::new(MacBackend::new(helper_path.clone(), synthetic));
    // Spawn the first sidecar pair now so its models start loading before the first meeting instead
    // of on the start path (subsequent pairs spawn in the background after each meeting adopts one).
    // Skipped while the models are still missing: those sidecars would download them themselves,
    // racing first-run setup over the same cache.
    if prewarm {
        backend.prewarm();
    }
    // Config defaults seed the orchestrator; the editable Settings panels override them per meeting
    // (read from the DB at start/stop). The refiner is always wired so toggling auto-refine on in the
    // UI takes effect — whether it runs at stop is gated by the effective setting — and so the manual
    // `/rediarize` route can drive the same refine path.
    // `into_arc` wraps the orchestrator and wires its weak self-reference in one step (via
    // `Arc::new_cyclic`), so a capture death (helper crash) can finalize the meeting instead of
    // leaving it falsely live — with no separate init call to forget.
    // Clone what the summarizer needs before `pool` / `notes_model` are moved below.
    let notes_pool = pool.clone();
    let notes_default_model = notes_model.clone();

    let orchestrator = Orchestrator::new(pool.clone(), output_dir, backend)
        .with_defaults(defaults)
        .with_refiner(Arc::new(MacRefiner {
            pool,
            diarize_path: helper_path.with_file_name("hearsay-diarize"),
            default_model: refine_model,
            timeout: refine_timeout,
        }));
    // Always wire the notes summarizer: it runs the local LLM out-of-process in the `hearsay-notes`
    // sidecar, so nothing links llama.cpp into this binary. Notes are available whenever the sidecar
    // binary + a notes model are present; otherwise the routes report unavailable and auto-notes is
    // skipped at runtime.
    let orchestrator = orchestrator.with_summarizer(Arc::new(SubprocessSummarizer {
        pool: notes_pool,
        notes_binary,
        default_model: notes_default_model,
        default_prompt: notes_prompt,
    }));
    let orchestrator = orchestrator.into_arc();
    // Start the background warm ticker: it re-warms the sidecar pool while idle (off the polled
    // `sidecars_ready` read) whenever the ANE is free. Must run within the Tokio runtime. Held back
    // until the models are on disk, so warming never races first-run setup for the same downloads.
    if prewarm {
        orchestrator.spawn_warm_ticker();
    }
    orchestrator
}
