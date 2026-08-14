//! First-run model setup. The installer ships no models, so the app fetches them once: the
//! FluidAudio live models through the `hearsay-models` sidecar (which calls the same loaders the
//! live sidecars call, so the prepared set cannot drift from the loaded set) and the whisper refine
//! model as a resumable, SHA256-verified download, plus an optional notes model in the same pass.
//!
//! `GET /api/setup` reports what is missing and how a run is progressing; `POST /api/setup` starts
//! one. Until it reports ready the UI blocks recording, because a meeting started without models
//! transcribes nothing.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Command;

use crate::config::Settings;
use crate::models;
use crate::schema::{DownloadStatus, SetupState, SetupStatus, SetupStep, SetupStepStatus};
use hearsay_engine::LiveEngine;

/// FluidAudio's cache layout: one directory per model repo under
/// `~/Library/Application Support/FluidAudio/Models`. All five present means the live sidecars load
/// locally instead of downloading mid-meeting; this list mirrors what `hearsay-models` prepares.
/// A name that drifts from FluidAudio only costs a redundant prepare run — the persisted
/// completion flag, not this probe, is what stops setup from repeating forever.
#[cfg(target_os = "macos")]
const FLUID_REPOS: &[&str] = &[
    "silero-vad",
    "speaker-diarization",
    "ls-eend/ami",
    "parakeet-tdt-0.6b-v3",
    "parakeet-unified-en-0.6b",
];

/// Approximate total size of the live models, for the progress bar before the sidecar's `plan` line
/// arrives with the real per-step weights.
const LIVE_APPROX_BYTES: i64 = 1_121 * 1_048_576;

const MB: i64 = 1_048_576;

/// Why a setup run could not be started.
pub enum StartError {
    UnknownModel,
    Busy,
}

/// One unit of work in a run: which asset to fetch and how.
enum Step {
    /// The FluidAudio live models, via the `hearsay-models` sidecar.
    Live,
    /// The whisper refine model, downloaded next to where the refine expects it.
    Refine(&'static models::Source, PathBuf),
    /// A notes model the user picked, downloaded into the models dir.
    Notes(&'static models::Source),
}

/// The run in flight (or the last one this process ran).
#[derive(Clone)]
struct Run {
    status: SetupStatus,
    steps: Vec<SetupStep>,
    message: Option<String>,
}

/// Owns the first-run probe and the single setup run. Held in `AppState` behind an `Arc`; the
/// background task updates the progress under the mutex as bytes arrive.
pub struct SetupManager {
    /// The `hearsay-models` sidecar (a sibling of the capture helper).
    prepare_bin: PathBuf,
    /// FluidAudio's model cache, where the live models land. `None` when `HOME` is unset.
    fluid_cache: Option<PathBuf>,
    /// Where a downloaded notes model goes.
    models_dir: PathBuf,
    /// Nothing to prepare: the scripted dev engine spawns no sidecars, so its browser test must not
    /// meet a setup gate for models it will never load.
    skip: bool,
    state: Arc<Mutex<Run>>,
}

impl SetupManager {
    pub fn new(settings: &Settings, skip: bool) -> Self {
        SetupManager {
            prepare_bin: settings.helper_path.with_file_name("hearsay-models"),
            fluid_cache: fluid_cache(settings),
            models_dir: settings.models_dir.clone(),
            skip,
            state: Arc::new(Mutex::new(Run {
                status: SetupStatus::Idle,
                steps: Vec::new(),
                message: None,
            })),
        }
    }

    /// Whether every model the app needs is on disk.
    pub fn models_present(&self, refine_model: &Path) -> bool {
        self.skip || (self.live_present() && models::is_ggml(refine_model))
    }

    fn live_present(&self) -> bool {
        live_models_present(self.fluid_cache.as_deref())
    }

    /// What the UI polls: whether setup is still needed, plus the run's per-step progress.
    pub async fn status(&self, pool: &SqlitePool, default_refine: &Path) -> SetupState {
        let refine = effective_refine(pool, default_refine).await;
        let run = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let complete = self.models_present(&refine) || self.recorded_complete(pool).await;

        match run.status {
            // A run is in flight (or failed): report its steps, and keep `required` true until the
            // models are actually there, so a failed run still blocks recording. A failure in the
            // optional notes step leaves `required` false — the app opens, and Settings > Models is
            // where that download is retried.
            SetupStatus::Running | SetupStatus::Error => SetupState {
                required: !complete,
                status: run.status,
                steps: run.steps,
                message: run.message,
            },
            _ if complete => SetupState {
                required: false,
                status: SetupStatus::Ready,
                steps: run.steps,
                message: None,
            },
            // Nothing running and models missing: advertise the work a run would do, so the setup
            // screen can state the download size before the user commits to it.
            _ => SetupState {
                required: true,
                status: SetupStatus::Idle,
                steps: self.planned_steps(&refine, None),
                message: None,
            },
        }
    }

    /// Start a run in the background (single-at-a-time), optionally fetching `notes_model_id` in the
    /// same pass. Steps already satisfied are skipped, so a retry after a partial failure resumes
    /// where it stopped.
    pub fn start(
        self: &Arc<Self>,
        pool: SqlitePool,
        default_refine: PathBuf,
        notes_model_id: Option<String>,
        engine: Arc<dyn LiveEngine>,
    ) -> Result<SetupState, StartError> {
        let notes = match notes_model_id.as_deref() {
            Some(id) => Some(models::notes_source(id).ok_or(StartError::UnknownModel)?),
            None => None,
        };
        {
            let mut run = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if run.status == SetupStatus::Running {
                return Err(StartError::Busy);
            }
            run.status = SetupStatus::Running;
            run.message = None;
            run.steps = Vec::new();
        }

        let manager = self.clone();
        tokio::spawn(async move {
            let refine = effective_refine(&pool, &default_refine).await;
            let steps = manager.planned_steps(&refine, notes);
            let work = manager.work_items(&refine, notes);
            {
                let mut run = manager.state.lock().unwrap_or_else(|e| e.into_inner());
                run.steps = steps;
            }
            manager.run_steps(work, pool, engine).await;
        });
        Ok(self.snapshot(true))
    }

    /// The steps a run would perform: only what is missing, so a machine that already has the live
    /// models sees just the refine download.
    fn planned_steps(
        &self,
        refine: &Path,
        notes: Option<&'static models::Source>,
    ) -> Vec<SetupStep> {
        let mut steps = Vec::new();
        if !self.live_present() {
            steps.push(pending_step("live", "Speech models", LIVE_APPROX_BYTES));
        }
        if !models::is_ggml(refine) {
            if let Some(source) = models::refine_source(refine) {
                steps.push(pending_step("refine", "Refine model", source.size_bytes()));
            }
        }
        if let Some(source) = notes {
            steps.push(pending_step("notes", "Notes model", source.size_bytes()));
        }
        steps
    }

    /// The same list as [`planned_steps`](Self::planned_steps), as the work to run.
    fn work_items(&self, refine: &Path, notes: Option<&'static models::Source>) -> Vec<Step> {
        let mut work = Vec::new();
        if !self.live_present() {
            work.push(Step::Live);
        }
        if !models::is_ggml(refine) {
            if let Some(source) = models::refine_source(refine) {
                let dir = refine
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.models_dir.clone());
                work.push(Step::Refine(source, dir));
            }
        }
        if let Some(source) = notes {
            work.push(Step::Notes(source));
        }
        work
    }

    /// Run each step in order, stopping at the first failure. On success the completion is recorded
    /// (so a probe that later disagrees cannot re-gate the app) and the engine starts pre-warming
    /// the sidecars it was held back from.
    async fn run_steps(&self, work: Vec<Step>, pool: SqlitePool, engine: Arc<dyn LiveEngine>) {
        for (index, step) in work.iter().enumerate() {
            self.set_step_status(index, SetupStepStatus::Running);
            let outcome = match step {
                Step::Live => self.run_prepare(index).await,
                Step::Refine(source, dir) => self.download(index, source, dir.clone(), None).await,
                Step::Notes(source) => {
                    let dir = self.models_dir.clone();
                    self.download(index, source, dir, Some(&pool)).await
                }
            };
            match outcome {
                Ok(()) => self.set_step_status(index, SetupStepStatus::Done),
                Err(reason) => {
                    tracing::warn!(step = index, error = %reason, "first-run setup failed");
                    self.set_step_status(index, SetupStepStatus::Error);
                    let mut run = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    run.status = SetupStatus::Error;
                    run.message = Some(reason);
                    return;
                }
            }
        }
        if let Err(err) = hearsay_db::queries::set_models_ready(&pool).await {
            tracing::warn!(error = ?err, "could not record setup completion");
        }
        {
            let mut run = self.state.lock().unwrap_or_else(|e| e.into_inner());
            run.status = SetupStatus::Ready;
        }
        tracing::info!("first-run setup complete");
        engine.start_prewarm();
    }

    /// Run the `hearsay-models` sidecar, mapping its NDJSON progress onto this step's byte counters.
    /// Its stderr (and FluidAudio's own logging) is inherited, so failures land in the core log.
    async fn run_prepare(&self, index: usize) -> Result<(), String> {
        let mut child = Command::new(&self.prepare_bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", self.prepare_bin.display()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "model preparation produced no output".to_string())?;

        let mut weights: Vec<(String, i64)> = Vec::new();
        let mut done_mb: i64 = 0;
        let mut finished = false;
        let mut failure: Option<String> = None;
        let mut lines = BufReader::new(stdout).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            // CoreML writes the odd diagnostic straight to stdout; anything that is not our NDJSON
            // is not progress.
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match event.get("kind").and_then(|k| k.as_str()) {
                Some("plan") => {
                    weights = event
                        .get("steps")
                        .and_then(|s| s.as_array())
                        .map(|steps| {
                            steps
                                .iter()
                                .filter_map(|s| {
                                    Some((
                                        s.get("id")?.as_str()?.to_string(),
                                        s.get("weight")?.as_i64()?,
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let total: i64 = weights.iter().map(|(_, w)| w).sum::<i64>() * MB;
                    self.set_step_bytes(index, 0, total.max(1));
                }
                Some("progress") => {
                    let id = event.get("step").and_then(|s| s.as_str()).unwrap_or("");
                    let fraction = event
                        .get("fraction")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0)
                        .clamp(0.0, 1.0);
                    let weight = weights
                        .iter()
                        .find(|(step, _)| step == id)
                        .map(|(_, w)| *w)
                        .unwrap_or(0);
                    let done = done_mb + (weight as f64 * fraction) as i64;
                    self.set_step_downloaded(index, done * MB);
                }
                Some("step") => {
                    let id = event.get("step").and_then(|s| s.as_str()).unwrap_or("");
                    done_mb += weights
                        .iter()
                        .find(|(step, _)| step == id)
                        .map(|(_, w)| *w)
                        .unwrap_or(0);
                    self.set_step_downloaded(index, done_mb * MB);
                }
                Some("error") => {
                    failure = Some(
                        event
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("model download failed")
                            .to_string(),
                    );
                }
                Some("done") => finished = true,
                _ => {}
            }
        }

        let status = child
            .wait()
            .await
            .map_err(|e| format!("model preparation did not exit cleanly: {e}"))?;
        if let Some(reason) = failure {
            return Err(reason);
        }
        if !status.success() || !finished {
            return Err(format!(
                "model preparation exited with {status} (see the log for details)"
            ));
        }
        Ok(())
    }

    /// Download one model file into `dir`, updating this step's byte counters. When `notes_pool` is
    /// set, the finished file becomes the effective notes model.
    async fn download(
        &self,
        index: usize,
        source: &'static models::Source,
        dir: PathBuf,
        notes_pool: Option<&SqlitePool>,
    ) -> Result<(), String> {
        let state = self.state.clone();
        let path = tokio::task::spawn_blocking(move || {
            models::download_source(source, &dir, &move |status, downloaded, total| {
                let mut run = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(step) = run.steps.get_mut(index) {
                    step.downloaded_bytes = downloaded as i64;
                    step.total_bytes = total as i64;
                    if status == DownloadStatus::Verifying {
                        step.status = SetupStepStatus::Verifying;
                    }
                }
            })
        })
        .await
        .map_err(|e| format!("download task panicked: {e}"))??;

        if let Some(pool) = notes_pool {
            if let Err(err) = hearsay_db::queries::set_notes_model(pool, &path).await {
                tracing::warn!(error = ?err, "downloaded notes model but could not select it");
            }
        }
        Ok(())
    }

    /// Whether a prior run recorded that setup finished. Belt to the probe's braces: if a cache
    /// folder name ever drifts from FluidAudio's, this is what keeps the app from gating forever on
    /// models it already has.
    async fn recorded_complete(&self, pool: &SqlitePool) -> bool {
        hearsay_db::queries::models_ready(pool)
            .await
            .unwrap_or(false)
    }

    fn snapshot(&self, required: bool) -> SetupState {
        let run = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        SetupState {
            required,
            status: run.status,
            steps: run.steps,
            message: run.message,
        }
    }

    fn set_step_status(&self, index: usize, status: SetupStepStatus) {
        let mut run = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(step) = run.steps.get_mut(index) {
            step.status = status;
            if status == SetupStepStatus::Done {
                step.downloaded_bytes = step.total_bytes;
            }
        }
    }

    fn set_step_bytes(&self, index: usize, downloaded: i64, total: i64) {
        let mut run = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(step) = run.steps.get_mut(index) {
            step.downloaded_bytes = downloaded;
            step.total_bytes = total;
        }
    }

    fn set_step_downloaded(&self, index: usize, downloaded: i64) {
        let mut run = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(step) = run.steps.get_mut(index) {
            step.downloaded_bytes = downloaded.min(step.total_bytes);
        }
    }
}

/// Whether every model the app needs is already on disk — the boot probe, which decides whether the
/// engine may pre-warm its sidecars. Pre-warming into a missing cache would have the live sidecars
/// download the very models a setup run downloads, over each other.
pub fn models_present(settings: &Settings, refine_model: &Path) -> bool {
    settings.scripted
        || (live_models_present(fluid_cache(settings).as_deref()) && models::is_ggml(refine_model))
}

/// FluidAudio's model cache for this user, where the live models land.
fn fluid_cache(settings: &Settings) -> Option<PathBuf> {
    settings
        .home_dir
        .as_ref()
        .map(|home| home.join("Library/Application Support/FluidAudio/Models"))
}

#[cfg(target_os = "macos")]
fn live_models_present(cache: Option<&Path>) -> bool {
    let Some(cache) = cache else {
        return false;
    };
    FLUID_REPOS
        .iter()
        .all(|repo| dir_has_entries(&cache.join(repo)))
}

/// Windows bundles its live (sherpa) models with the installer, so there is nothing to fetch.
#[cfg(not(target_os = "macos"))]
fn live_models_present(_cache: Option<&Path>) -> bool {
    true
}

/// The refine model the app will actually load: the stored Models-panel override, else the config
/// default. Setup fetches that file, not the default, so an install pointed elsewhere is not left
/// downloading a model it will never open.
async fn effective_refine(pool: &SqlitePool, default: &Path) -> PathBuf {
    hearsay_db::queries::effective_refine_model(pool, default)
        .await
        .unwrap_or_else(|_| default.to_path_buf())
}

fn pending_step(id: &str, label: &str, total_bytes: i64) -> SetupStep {
    SetupStep {
        id: id.to_string(),
        label: label.to_string(),
        status: SetupStepStatus::Pending,
        downloaded_bytes: 0,
        total_bytes,
    }
}

/// Whether `dir` exists and holds at least one entry — a model repo that was created but never
/// filled (an interrupted download) does not count as present.
#[cfg(target_os = "macos")]
fn dir_has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn dir_has_entries_rejects_missing_and_empty_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!dir_has_entries(&tmp.path().join("nope")));
        let empty = tmp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(!dir_has_entries(&empty));
        std::fs::write(empty.join("model.bin"), b"x").unwrap();
        assert!(dir_has_entries(&empty));
    }
}
