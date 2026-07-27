//! The Windows live-capture + offline-refine backend, assembled into an [`Orchestrator`] by
//! [`build_engine`]: WASAPI capture
//! ([`WasapiSource`]), live captions on both streams via the sherpa streaming ASR
//! ([`SherpaTranscriber`], speaker-less — the refine assigns speakers), and an offline refine of
//! whisper + the sherpa pyannote diarizer ([`WindowsRefiner`]). Everything runs in-process: there
//! are no sidecar processes, so there is no warm pool — the streaming model is loaded once here
//! and shared by every meeting.
//!
//! The live/diarize models are conventional filenames under `EngineConfig::sherpa_models_dir`
//! (bundled by the installer, `HEARSAY_SHERPA_MODELS_DIR`). If the streaming model cannot load,
//! the engine degrades to [`DisabledEngine`] (lifecycle routes 503) rather than failing startup,
//! so the rest of the app — history, search, settings, downloads — stays usable.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use hearsay_capture::{LoopbackMode, SyntheticSource, WasapiSource};
use hearsay_engine::{DisabledEngine, LiveEngine};
use hearsay_inference::{
    PunctuationModel, Punctuator, SherpaDiarizer, StreamingAsr, StreamingModel,
};
use hearsay_orchestrator::{
    Backend, BackendInstance, Orchestrator, OrchestratorError, RefineResult, RefinedThemSegment,
    Refiner,
};

use crate::summarizer::SubprocessSummarizer;
use crate::{EngineConfig, SherpaTranscriber};

/// The streaming-ASR model directory under `sherpa_models_dir` (the model validated by
/// `hearsay-inference/tests/sherpa_streaming.rs`). The 70M LibriSpeech+GigaSpeech zipformer: the
/// 20M it replaced dropped whole leading clauses on clean speech, and `tests/streaming_bench.rs`
/// measures this one at RTF 0.031 — 20 ms of compute per 560 ms of audio, so both streams still
/// run live with room to spare.
const STREAMING_DIR: &str = "sherpa-onnx-streaming-zipformer-en-2023-06-21";
/// The online punctuation model directory under `sherpa_models_dir`. The streaming zipformer emits
/// bare uppercase text; this restores the case + punctuation macOS gets natively from Parakeet.
const PUNCT_DIR: &str = "sherpa-onnx-online-punct-en-2024-08-06";
/// The pyannote segmentation model under `sherpa_models_dir`.
const SEGMENTATION_MODEL: &str = "sherpa-onnx-pyannote-segmentation-3-0/model.onnx";
/// The speaker-embedding model under `sherpa_models_dir` — TitaNet small, the embedder the
/// `sweep_cluster_threshold` tuning (and `DEFAULT_CLUSTER_THRESHOLD`) was established with.
const EMBEDDING_MODEL: &str = "nemo_en_titanet_small.onnx";

/// The Windows live backend: WASAPI capture + one shared streaming recognizer driving a
/// [`SherpaTranscriber`] per stream. No processes to pool; `sidecars_ready` is true from load.
struct WindowsBackend {
    asr: StreamingAsr,
    /// Restores case + punctuation on the zipformer's uppercase output; `None` when the model is
    /// absent (older bundle), which keeps live captions working in raw uppercase.
    punct: Option<Punctuator>,
    synthetic: bool,
    loopback_mode: LoopbackMode,
}

impl Backend for WindowsBackend {
    fn build(&self) -> BackendInstance {
        let source: Box<dyn hearsay_orchestrator::AudioSource> = if self.synthetic {
            Box::new(SyntheticSource::new())
        } else {
            Box::new(WasapiSource::new(self.loopback_mode))
        };
        BackendInstance {
            source,
            me: Box::new(SherpaTranscriber::new(self.asr.clone(), self.punct.clone())),
            them: Box::new(SherpaTranscriber::new(self.asr.clone(), self.punct.clone())),
        }
    }
}

/// The post-meeting offline refine: re-diarize the Them track with the sherpa pyannote diarizer
/// and re-transcribe with whisper, through the shared `refine_audio_file_with` seam. The diarizer
/// is constructed inside the blocking task (created, used, and dropped on one thread), so its ONNX
/// handles never cross threads.
struct WindowsRefiner {
    pool: SqlitePool,
    /// Bundled config default; the effective model is the `models` preference override else this,
    /// resolved from the DB at each refine so a Models-panel change applies with no restart.
    default_model: PathBuf,
    segmentation_model: PathBuf,
    embedding_model: PathBuf,
}

#[async_trait]
impl Refiner for WindowsRefiner {
    async fn refine(&self, audio_path: &Path) -> Result<RefineResult, OrchestratorError> {
        for (model, what) in [
            (&self.segmentation_model, "segmentation"),
            (&self.embedding_model, "speaker-embedding"),
        ] {
            if !model.is_file() {
                return Err(OrchestratorError::Backend(format!(
                    "{what} model not found at {}",
                    model.display()
                )));
            }
        }
        let model = hearsay_db::queries::effective_refine_model(&self.pool, &self.default_model)
            .await
            .map_err(|e| OrchestratorError::Backend(format!("resolve refine model: {e}")))?;
        let audio = audio_path.to_path_buf();
        let segmentation = self.segmentation_model.clone();
        let embedding = self.embedding_model.clone();
        // whisper + the ONNX diarizer are blocking — run off the async runtime.
        let output = tokio::task::spawn_blocking(move || {
            let diarizer = SherpaDiarizer::load(&segmentation, &embedding)?;
            hearsay_inference::refine_audio_file_with(&audio, &diarizer, &model)
        })
        .await
        .map_err(|e| OrchestratorError::Backend(format!("refine task panicked: {e}")))?;
        let output = match output {
            Ok(output) => output,
            // A silent / Me-only meeting has no remote speech to refine; benign no-op (matching
            // the macOS refiner) — the caller keeps the existing segments.
            Err(hearsay_inference::InferenceError::NoSpeech) => return Ok(RefineResult::default()),
            Err(e) => return Err(OrchestratorError::Backend(format!("refine failed: {e}"))),
        };
        Ok(RefineResult {
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
        })
    }
}

/// Load the streaming zipformer from its conventional directory under `sherpa_models_dir`.
fn load_streaming_asr(models_dir: &Path) -> Result<StreamingAsr, String> {
    let dir = models_dir.join(STREAMING_DIR);
    let encoder = dir.join("encoder-epoch-99-avg-1.int8.onnx");
    let decoder = dir.join("decoder-epoch-99-avg-1.int8.onnx");
    let joiner = dir.join("joiner-epoch-99-avg-1.int8.onnx");
    let tokens = dir.join("tokens.txt");
    for file in [&encoder, &decoder, &joiner, &tokens] {
        if !file.is_file() {
            return Err(format!(
                "streaming model file not found at {}",
                file.display()
            ));
        }
    }
    StreamingAsr::load(StreamingModel {
        encoder: &encoder,
        decoder: &decoder,
        joiner: &joiner,
        tokens: &tokens,
    })
    .map_err(|e| format!("load streaming ASR: {e}"))
}

/// Load the online punctuation model from its conventional directory under `sherpa_models_dir`.
/// `None` when it is not bundled: live captions then read as raw uppercase, which is worse but not
/// broken, so a missing punctuation model must never take live transcription down with it.
fn load_punctuator(models_dir: &Path) -> Option<Punctuator> {
    let dir = models_dir.join(PUNCT_DIR);
    let model = dir.join("model.int8.onnx");
    let vocab = dir.join("bpe.vocab");
    if !model.is_file() || !vocab.is_file() {
        tracing::warn!(
            dir = %dir.display(),
            "punctuation model not found; live captions will be uppercase without punctuation"
        );
        return None;
    }
    match Punctuator::load(PunctuationModel {
        model: &model,
        vocab: &vocab,
    }) {
        Ok(punct) => Some(punct),
        Err(err) => {
            tracing::warn!(error = %err, "punctuation model failed to load; live captions will be uppercase");
            None
        }
    }
}

/// Assemble the Windows live engine: WASAPI capture + sherpa live captions ([`WindowsBackend`])
/// and the whisper + sherpa-diarize offline refine ([`WindowsRefiner`]) inside an
/// [`Orchestrator`], returned as the neutral [`LiveEngine`] the HTTP crate consumes. A failed
/// streaming-model load logs the reason and returns [`DisabledEngine`] instead (the app serves;
/// meetings 503). Call from within the Tokio runtime.
pub fn build_engine(config: EngineConfig) -> Arc<dyn LiveEngine> {
    let EngineConfig {
        pool,
        output_dir,
        helper_path: _, // macOS-only: there is no helper process on Windows
        synthetic,
        refine_model,
        refine_timeout: _, // macOS-only: bounds the diarize subprocess; the ONNX diarizer is in-process
        record,
        auto_refine,
        recognition_threshold,
        inactivity_prompt,
        inactivity_auto_end,
        inactivity_prompt_minutes,
        inactivity_end_minutes,
        notes_enabled,
        notes_model,
        notes_prompt,
        notes_binary,
        sherpa_models_dir,
        win_loopback_mode,
    } = config;

    let asr = match load_streaming_asr(&sherpa_models_dir) {
        Ok(asr) => asr,
        Err(err) => {
            tracing::error!(
                error = %err,
                dir = %sherpa_models_dir.display(),
                "live transcription unavailable: streaming model failed to load; serving without meetings"
            );
            return Arc::new(DisabledEngine);
        }
    };
    tracing::info!(dir = %sherpa_models_dir.display(), loopback = ?win_loopback_mode, "windows backend ready");

    let backend = Arc::new(WindowsBackend {
        asr,
        punct: load_punctuator(&sherpa_models_dir),
        synthetic,
        loopback_mode: win_loopback_mode,
    });

    let notes_pool = pool.clone();
    let notes_default_model = notes_model.clone();

    let orchestrator = Orchestrator::new(pool.clone(), output_dir, backend)
        .with_defaults(
            record,
            auto_refine,
            recognition_threshold,
            inactivity_prompt,
            inactivity_auto_end,
            inactivity_prompt_minutes,
            inactivity_end_minutes,
            notes_enabled,
            notes_model,
        )
        .with_refiner(Arc::new(WindowsRefiner {
            pool,
            default_model: refine_model,
            segmentation_model: sherpa_models_dir.join(SEGMENTATION_MODEL),
            embedding_model: sherpa_models_dir.join(EMBEDDING_MODEL),
        }));
    let orchestrator = orchestrator.with_summarizer(Arc::new(SubprocessSummarizer {
        pool: notes_pool,
        notes_binary,
        default_model: notes_default_model,
        default_prompt: notes_prompt,
    }));
    orchestrator.into_arc()
}
