//! Hearsay application binary: resolve settings, open the database, bind the loopback listener, and
//! serve the API + UI. `--dump-openapi` prints the OpenAPI document and exits (for the TS codegen);
//! `--synthetic` runs the capture helper in synthetic mode (generated audio, no TCC prompts).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpListener;
use utoipa::OpenApi as _;
use uuid::Uuid;

use hearsay_capture::SwiftHelperSource;
use hearsay_core::{create_app, ApiDoc, AppState, LiveEngine as _, Settings};
use hearsay_orchestrator::{
    Backend, BackendInstance, Orchestrator, OrchestratorError, ProcessTranscriber, RefineResult,
    RefinedThemSegment, Refiner,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
/// into the orchestrator so a meeting auto-refines at stop (the same path as the `/rediarize`
/// button).
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
        .map_err(|e| OrchestratorError::Backend(format!("refine task panicked: {e}")))?
        .map_err(|e| OrchestratorError::Backend(format!("refine failed: {e}")))?;
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

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    if std::env::args().any(|arg| arg == "--dump-openapi") {
        println!("{}", ApiDoc::openapi().to_pretty_json()?);
        return Ok(());
    }
    let synthetic = std::env::args().any(|arg| arg == "--synthetic");

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // Include the orchestrator + capture crates so sidecar stderr (forwarded via tracing)
                // and lifecycle warnings surface by default, not only under RUST_LOG.
                "hearsay_core=info,hearsay_orchestrator=info,hearsay_capture=info,tower_http=info"
                    .into()
            }),
        )
        .init();

    let settings = Settings::from_env();
    // Refuse a public bind outside development before doing any work (loopback is not a boundary, but
    // a network-reachable bind exposes the token-gated API to everyone).
    settings.ensure_bind_allowed()?;
    let is_dev = settings.environment == "development";
    let pool = hearsay_db::connect(&settings.database_url).await?;
    // 244-bit hex session token (two v4 UUIDs; each contributes 122 random bits).
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());

    let bind = format!("{}:{}", settings.server_host, settings.server_port);
    let backend = Arc::new(MacBackend::new(settings.helper_path.clone(), synthetic));
    // Seed FluidAudio's model cache from the bundled copy before spawning any sidecar, so the live
    // models load locally instead of downloading from HuggingFace on the first meeting. One-time
    // (copies only what's missing), so it's a fast no-op after the first launch.
    seed_fluid_models();
    // Spawn the first sidecar pair now so its models start loading before the first meeting instead
    // of on the start path (subsequent pairs spawn in the background after each meeting adopts one).
    backend.prewarm();
    // Config defaults seed the orchestrator; the editable Settings panels override them per meeting
    // (read from the DB at start/stop). The refiner is always made available so toggling auto-refine
    // on in the UI takes effect — whether it runs at stop is gated by the effective setting.
    let orchestrator = Orchestrator::new(pool.clone(), settings.output_dir.clone(), backend)
        .with_defaults(
            settings.record,
            settings.auto_refine,
            settings.recognition_threshold,
        )
        .with_refiner(Arc::new(MacRefiner {
            pool: pool.clone(),
            diarize_path: settings.helper_path.with_file_name("hearsay-diarize"),
            default_model: settings.refine_model.clone(),
            timeout: settings.refine_timeout,
        }));
    // Keep the concrete `Arc<Orchestrator>` so the shutdown path below can stop the active meeting
    // and await any in-flight refine; it coerces to `Arc<dyn LiveEngine>` for the app state.
    let orchestrator = Arc::new(orchestrator);
    // Give the orchestrator its own `Arc` handle so a capture death (helper crash) can finalize the
    // meeting instead of leaving it falsely live.
    orchestrator.install_self();
    let state = AppState::new(pool, settings, token.clone(), orchestrator.clone());
    let app = create_app(state);

    let listener = TcpListener::bind(&bind).await?;
    let addr = listener.local_addr()?;
    // Hand the shell the resolved port + token via a private 0600 file instead of stdout, so the
    // token never lands in a log line. No-op when HEARSAY_HANDSHAKE_PATH is unset (headless dev).
    write_handshake(addr.port(), &token)?;
    // Print the open URL (which carries the token) only in development; production uses the handshake.
    if is_dev {
        println!("open: http://{addr}/?token={token}");
    }
    tracing::info!(%addr, synthetic, "hearsay core listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // The listener has stopped (a shutdown signal fired). Stop the active meeting so it is finalized
    // and its audio/transcript flushed before we exit — otherwise a shell crash (which closes our
    // stdin -> shutdown) would strand a `recording` row and orphan the sidecars. Then wait for any
    // in-flight refine so the background task is not cut off mid-write.
    if let Some(id) = orchestrator.active_meeting() {
        tracing::info!(meeting = %id, "stopping active meeting on shutdown");
        if let Err(err) = orchestrator.stop_meeting(id).await {
            tracing::warn!(error = ?err, "failed to stop active meeting on shutdown");
        }
    }
    orchestrator.wait_for_refines().await;
    Ok(())
}

/// Write `{port, token}` JSON to the path named by `HEARSAY_HANDSHAKE_PATH` — the private file the
/// desktop shell reads once to navigate the webview. Written via temp file + rename (0600 on Unix)
/// so the shell never reads a half-written payload. A no-op when the env var is unset (headless dev).
fn write_handshake(port: u16, token: &str) -> std::io::Result<()> {
    let Some(path) = std::env::var_os("HEARSAY_HANDSHAKE_PATH") else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    let payload = serde_json::json!({ "port": port, "token": token }).to_string();

    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write as _;
        let mut file = options.open(&tmp)?;
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Seed FluidAudio's model cache from the bundled copy so the live sidecars load their models
/// locally instead of downloading them from HuggingFace — a self-contained, offline install. Reads
/// `HEARSAY_FLUID_MODELS_DIR` (set by the desktop shell to the bundled resource); a no-op when unset
/// (headless dev, where FluidAudio downloads to its cache as before). Copies each model repo only
/// when it is absent from the cache, so it runs once (first launch, or after an erase wipes the
/// cache) and is a fast no-op afterward. Each repo is copied into a hidden `.partial` dir and then
/// atomically renamed into place, so an interrupted copy never leaves a half-tree the sidecar would
/// try to load. Best-effort: a failure is logged, not fatal — the sidecar then downloads that repo.
fn seed_fluid_models() {
    let Some(src) = std::env::var_os("HEARSAY_FLUID_MODELS_DIR") else {
        return; // headless dev: FluidAudio downloads to its own cache as before
    };
    let src = PathBuf::from(src);
    if !src.is_dir() {
        return; // no bundled models (e.g. a dev build packaged without them)
    }
    let Some(home) = std::env::var_os("HOME") else {
        tracing::warn!("HOME unset; cannot seed FluidAudio models from the bundle");
        return;
    };
    // FluidAudio's default cache: ~/Library/Application Support/FluidAudio/Models/<repo>/.
    let dest = PathBuf::from(home).join("Library/Application Support/FluidAudio/Models");
    seed_models_into(&src, &dest);
}

/// Copy each model-repo subdirectory of `src` into `dest`, skipping any already present there (so it
/// only does work on the first launch / after an erase). Each repo is copied into a hidden `.partial`
/// dir and then atomically renamed into place, so an interrupted copy never leaves a half-tree the
/// sidecar would try to load. Best-effort: a failure is logged, not fatal — the sidecar then
/// downloads that repo. Split from [`seed_fluid_models`] (which resolves the paths from the env) so
/// the copy logic is unit-testable against temp dirs.
fn seed_models_into(src: &Path, dest: &Path) {
    let entries = match std::fs::read_dir(src) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(error = %err, dir = %src.display(), "could not read bundled FluidAudio models");
            return;
        }
    };
    for entry in entries.flatten() {
        // Only model-repo subdirectories (skip stray files).
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let target = dest.join(&name);
        if target.exists() {
            continue; // already cached -> the sidecar loads it and skips the download
        }
        let tmp = dest.join(format!(".{}.partial", name.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&tmp); // clear any stale partial from a prior interrupted run
        let seeded =
            copy_dir_all(&entry.path(), &tmp).and_then(|()| std::fs::rename(&tmp, &target));
        match seeded {
            Ok(()) => {
                tracing::info!(model = %name.to_string_lossy(), "seeded FluidAudio model from bundle")
            }
            Err(err) => {
                let _ = std::fs::remove_dir_all(&tmp);
                tracing::warn!(error = %err, model = %name.to_string_lossy(), "failed to seed FluidAudio model; the sidecar will download it");
            }
        }
    }
}

/// Recursively copy the directory `src` into `dst` (creating `dst`). A small std-only helper for
/// [`seed_fluid_models`]; the model repos contain only files and subdirectories (no symlinks).
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Resolve when any shutdown trigger fires: Ctrl-C, SIGTERM (Unix), or EOF on our stdin. The desktop
/// shell holds our stdin, so stdin EOF is how a shell quit (or crash) tells us to exit — this is the
/// parent-death signal that keeps a headless core with a live token from lingering.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    let stdin_eof = async {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 64];
        // Read (and discard) until EOF; the shell never writes to our stdin, so this only completes
        // when the pipe closes.
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // If we cannot install the handler, never fire this arm.
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("shutdown: ctrl-c"),
        _ = stdin_eof => tracing::info!("shutdown: stdin closed (parent exited)"),
        _ = terminate => tracing::info!("shutdown: SIGTERM"),
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_dir_all, seed_models_into};
    use std::fs;

    /// Recursively snapshot a directory into sorted `relative/path -> contents` pairs, for asserting
    /// a copy reproduced the tree exactly.
    fn tree(root: &std::path::Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    let rel = path
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    out.push((rel, fs::read_to_string(&path).unwrap()));
                }
            }
        }
        walk(root, root, &mut out);
        out.sort();
        out
    }

    #[test]
    fn copy_dir_all_reproduces_a_nested_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("sub/b.txt"), "b").unwrap();

        let dst = tmp.path().join("dst");
        copy_dir_all(&src, &dst).unwrap();

        assert_eq!(tree(&src), tree(&dst));
    }

    #[test]
    fn seed_copies_missing_repos_and_skips_present_ones() {
        let tmp = tempfile::tempdir().unwrap();
        // Bundled source: two model repos.
        let src = tmp.path().join("bundle/Models");
        fs::create_dir_all(src.join("repo-a")).unwrap();
        fs::write(src.join("repo-a/model.bin"), "AAA").unwrap();
        fs::create_dir_all(src.join("repo-b")).unwrap();
        fs::write(src.join("repo-b/model.bin"), "BBB").unwrap();
        // A stray file at the source root must be ignored (only repo dirs are seeded).
        fs::write(src.join("README"), "ignore me").unwrap();

        // Cache: repo-a is already present (with different bytes) and must NOT be overwritten.
        let dest = tmp.path().join("cache/Models");
        fs::create_dir_all(dest.join("repo-a")).unwrap();
        fs::write(dest.join("repo-a/model.bin"), "existing").unwrap();

        seed_models_into(&src, &dest);

        // repo-b was copied; repo-a was left untouched; the stray file was not copied; no .partial left.
        assert_eq!(
            fs::read_to_string(dest.join("repo-b/model.bin")).unwrap(),
            "BBB"
        );
        assert_eq!(
            fs::read_to_string(dest.join("repo-a/model.bin")).unwrap(),
            "existing"
        );
        assert!(!dest.join("README").exists());
        let leftovers: Vec<_> = fs::read_dir(&dest)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("partial"))
            .collect();
        assert!(leftovers.is_empty(), "left a partial dir: {leftovers:?}");
    }

    #[test]
    fn seed_into_missing_source_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("cache/Models");
        // A nonexistent source must not panic and must not create the destination.
        seed_models_into(&tmp.path().join("does-not-exist"), &dest);
        assert!(!dest.exists());
    }
}
