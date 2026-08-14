//! First-run model setup: the live models via the `hearsay-models` sidecar, the whisper refine
//! model (and optionally a notes model) via the shared downloader. Drives `GET/POST /api/setup`.

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

/// FluidAudio's cache layout: one directory per repo. A name that drifts only costs a redundant
/// prepare run — the persisted completion flag is what stops setup repeating.
#[cfg(target_os = "macos")]
const FLUID_REPOS: &[&str] = &[
    "silero-vad",
    "speaker-diarization",
    "ls-eend/ami",
    "parakeet-tdt-0.6b-v3",
    "parakeet-unified-en-0.6b",
];

/// Stand-in total until the sidecar's `plan` line arrives with the real per-step weights.
const LIVE_APPROX_BYTES: i64 = 1_121 * 1_048_576;

const MB: i64 = 1_048_576;

/// Why a setup run could not be started.
pub enum StartError {
    UnknownModel,
    Busy,
}

/// One unit of work in a run. `Refine` carries the directory the refine loads from.
enum Step {
    Live,
    Refine(&'static models::Source, PathBuf),
    Notes(&'static models::Source),
}

/// The run in flight (or the last one this process ran).
#[derive(Clone)]
struct Run {
    status: SetupStatus,
    steps: Vec<SetupStep>,
    message: Option<String>,
}

/// Owns the first-run probe and the single setup run. Held in `AppState` behind an `Arc`.
pub struct SetupManager {
    /// The `hearsay-models` sidecar (a sibling of the capture helper).
    prepare_bin: PathBuf,
    /// FluidAudio's model cache. `None` when `HOME` is unset.
    fluid_cache: Option<PathBuf>,
    models_dir: PathBuf,
    /// The scripted dev engine spawns no sidecars, so it must not meet a setup gate.
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
        present(self.skip, self.fluid_cache.as_deref(), refine_model)
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
            // `required` tracks the models, not the run, so a failure still blocks recording — and a
            // failed *notes* step (optional) does not.
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
            // Advertise the work a run would do, so the screen can state the size up front.
            _ => SetupState {
                required: true,
                status: SetupStatus::Idle,
                steps: self
                    .plan(&refine, None)
                    .into_iter()
                    .map(|(s, _)| s)
                    .collect(),
                message: None,
            },
        }
    }

    /// Start a run in the background (single-at-a-time). Satisfied steps are skipped, so a retry
    /// after a partial failure resumes where it stopped.
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
            let (steps, work): (Vec<_>, Vec<_>) = manager.plan(&refine, notes).into_iter().unzip();
            {
                let mut run = manager.state.lock().unwrap_or_else(|e| e.into_inner());
                run.steps = steps;
            }
            manager.run_steps(work, pool, engine).await;
        });
        Ok(self.snapshot(true))
    }

    /// What a run would do — only what is missing. The reported step and the work that fills it are
    /// produced together: `run_steps` indexes the progress by position, so they cannot be two lists
    /// that drift apart.
    fn plan(
        &self,
        refine: &Path,
        notes: Option<&'static models::Source>,
    ) -> Vec<(SetupStep, Step)> {
        let mut plan = Vec::new();
        if !self.live_present() {
            plan.push((
                pending_step("live", "Speech models", LIVE_APPROX_BYTES),
                Step::Live,
            ));
        }
        if !models::is_ggml(refine) {
            if let Some(source) = models::refine_source(refine) {
                let dir = refine
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.models_dir.clone());
                plan.push((
                    pending_step("refine", "Refine model", source.size_bytes()),
                    Step::Refine(source, dir),
                ));
            }
        }
        if let Some(source) = notes {
            plan.push((
                pending_step("notes", "Notes model", source.size_bytes()),
                Step::Notes(source),
            ));
        }
        plan
    }

    /// Run each step in order, stopping at the first failure. Success records the completion and
    /// releases the pre-warm the engine was held back from.
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
    /// stderr is inherited, so its (and FluidAudio's) logging lands in the core log.
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
            // CoreML writes the odd diagnostic straight to stdout; skip anything that is not ours.
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

    /// Download one model into `dir`. With `notes_pool` set, the file becomes the notes model.
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
                update_step(&state, index, |step| {
                    step.downloaded_bytes = downloaded as i64;
                    step.total_bytes = total as i64;
                    if status == DownloadStatus::Verifying {
                        step.status = SetupStepStatus::Verifying;
                    }
                });
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

    /// Whether a prior run recorded that setup finished — what keeps a drifted [`FLUID_REPOS`] name
    /// from gating the app forever on models it already has.
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
        update_step(&self.state, index, |step| {
            step.status = status;
            if status == SetupStepStatus::Done {
                step.downloaded_bytes = step.total_bytes;
            }
        });
    }

    fn set_step_bytes(&self, index: usize, downloaded: i64, total: i64) {
        update_step(&self.state, index, |step| {
            step.downloaded_bytes = downloaded;
            step.total_bytes = total;
        });
    }

    fn set_step_downloaded(&self, index: usize, downloaded: i64) {
        update_step(&self.state, index, |step| {
            step.downloaded_bytes = downloaded.min(step.total_bytes);
        });
    }
}

/// The boot probe: whether every model is on disk, which decides whether the engine may pre-warm.
/// Runs before there is an [`AppState`](crate::AppState) to ask, and answers the same rule the
/// manager's [`models_present`](SetupManager::models_present) does.
pub fn models_present(settings: &Settings, refine_model: &Path) -> bool {
    present(
        settings.scripted,
        fluid_cache(settings).as_deref(),
        refine_model,
    )
}

fn present(skip: bool, cache: Option<&Path>, refine_model: &Path) -> bool {
    skip || (live_models_present(cache) && models::is_ggml(refine_model))
}

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

/// Windows bundles its sherpa models, so there is nothing to fetch.
#[cfg(not(target_os = "macos"))]
fn live_models_present(_cache: Option<&Path>) -> bool {
    true
}

/// The refine model the app will load: the stored override, else the config default. Setup fetches
/// that file, not the default.
async fn effective_refine(pool: &SqlitePool, default: &Path) -> PathBuf {
    hearsay_db::queries::effective_refine_model(pool, default)
        .await
        .unwrap_or_else(|_| default.to_path_buf())
}

/// Mutate one step under the lock. An index past the end is ignored, so a progress line arriving
/// after its run was replaced cannot panic.
fn update_step(state: &Mutex<Run>, index: usize, edit: impl FnOnce(&mut SetupStep)) {
    let mut run = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(step) = run.steps.get_mut(index) {
        edit(step);
    }
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

/// A repo dir created but never filled (an interrupted download) does not count as present.
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
