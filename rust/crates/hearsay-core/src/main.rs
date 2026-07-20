//! Hearsay application binary: resolve settings, open the database, bind the loopback listener, and
//! serve the API + UI. `--dump-openapi` prints the OpenAPI document and exits (for the TS codegen);
//! `--synthetic` runs the capture helper in synthetic mode (generated audio, no TCC prompts).

use std::path::Path;

use tokio::io::AsyncReadExt as _;
use tokio::net::TcpListener;
use utoipa::OpenApi as _;
use uuid::Uuid;

use hearsay_backends::{build_engine, EngineConfig};
use hearsay_core::{create_app, ApiDoc, AppState, Settings};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
                // Include the backends + orchestrator + capture crates so sidecar stderr (forwarded
                // via tracing) and lifecycle warnings surface by default, not only under RUST_LOG.
                "hearsay_core=info,hearsay_backends=info,hearsay_orchestrator=info,hearsay_capture=info,tower_http=info"
                    .into()
            }),
        )
        .init();

    let settings = Settings::from_env()?;
    // Refuse a public bind outside development before doing any work (loopback is not a boundary, but
    // a network-reachable bind exposes the token-gated API to everyone).
    settings.ensure_bind_allowed()?;
    let is_dev = settings.environment == "development";
    let pool = hearsay_db::connect(&settings.database_url).await?;
    // 244-bit hex session token (two v4 UUIDs; each contributes 122 random bits).
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());

    let bind = format!("{}:{}", settings.server_host, settings.server_port);
    // Seed FluidAudio's model cache from the bundled copy before spawning any sidecar, so the live
    // models load locally instead of downloading from HuggingFace on the first meeting. One-time
    // (copies only what's missing), so it's a fast no-op after the first launch.
    #[cfg(target_os = "macos")]
    seed_fluid_models(
        settings.fluid_models_dir.as_deref(),
        settings.home_dir.as_deref(),
    );
    // Assemble the platform backend behind the neutral LiveEngine seam (macOS: capture helper +
    // live sidecars + whisper refine; Windows: WASAPI capture + the sherpa path). `build_engine`
    // installs the orchestrator's self-reference (so a capture death finalizes the meeting); the
    // binary holds only the trait object. Config defaults seed it; the editable Settings panels
    // override per meeting.
    let engine = build_engine(EngineConfig {
        pool: pool.clone(),
        output_dir: settings.output_dir.clone(),
        helper_path: settings.helper_path.clone(),
        synthetic,
        refine_model: settings.refine_model.clone(),
        refine_timeout: settings.refine_timeout,
        record: settings.record,
        auto_refine: settings.auto_refine,
        recognition_threshold: settings.recognition_threshold,
        inactivity_prompt: settings.inactivity_prompt,
        inactivity_auto_end: settings.inactivity_auto_end,
        inactivity_prompt_minutes: settings.inactivity_prompt_minutes,
        inactivity_end_minutes: settings.inactivity_end_minutes,
        notes_enabled: settings.notes_enabled,
        notes_model: settings.notes_model.clone(),
        notes_prompt: settings.notes_prompt.clone(),
        sherpa_models_dir: settings.sherpa_models_dir.clone(),
        win_loopback_mode: settings.win_loopback_mode,
    });
    // A prior hard exit (SIGKILL / panic / power loss) can strand a meeting row `recording` or
    // `refining` forever, with no session to finalize it. Nothing is active at startup, so sweep and
    // finalize every such row (writing its transcript from the persisted segments) before we serve.
    hearsay_backends::reconcile::reconcile_stranded_meetings(&pool, &settings.output_dir).await;

    // Grab the handshake path before `settings` moves into the app state; the handshake file is
    // written after the listener binds (it carries the resolved port).
    let handshake_path = settings.handshake_path.clone();
    let state = AppState::new(pool, settings, token.clone(), engine.clone());
    let app = create_app(state);

    let listener = TcpListener::bind(&bind).await?;
    let addr = listener.local_addr()?;
    // Hand the shell the resolved port + token via a private 0600 file instead of stdout, so the
    // token never lands in a log line. No-op when the handshake path is unset (headless dev).
    write_handshake(addr.port(), &token, handshake_path.as_deref())?;
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
    if let Some(id) = engine.active_meeting() {
        tracing::info!(meeting = %id, "stopping active meeting on shutdown");
        if let Err(err) = engine.stop_meeting(id).await {
            tracing::warn!(error = ?err, "failed to stop active meeting on shutdown");
        }
    }
    engine.shutdown().await;
    Ok(())
}

/// Write `{port, token}` JSON to `handshake_path` (resolved from `HEARSAY_HANDSHAKE_PATH` into
/// [`Settings`]) — the private file the desktop shell reads once to navigate the webview. Written via
/// temp file + rename (0600 on Unix) so the shell never reads a half-written payload. A no-op when
/// the path is `None` (headless dev).
fn write_handshake(port: u16, token: &str, handshake_path: Option<&Path>) -> std::io::Result<()> {
    let Some(path) = handshake_path else {
        return Ok(());
    };
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
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Seed FluidAudio's model cache from the bundled copy so the live sidecars load their models
/// locally instead of downloading them from HuggingFace — a self-contained, offline install. Takes
/// the bundled-models dir (`HEARSAY_FLUID_MODELS_DIR`, set by the desktop shell) and `HOME`, both
/// resolved into [`Settings`]; a no-op when the models dir is `None` (headless dev, where FluidAudio
/// downloads to its cache as before). Copies each model repo only when it is absent from the cache,
/// so it runs once (first launch, or after an erase wipes the cache) and is a fast no-op afterward.
/// Each repo is copied into a hidden `.partial` dir and then atomically renamed into place, so an
/// interrupted copy never leaves a half-tree the sidecar would try to load. Best-effort: a failure is
/// logged, not fatal — the sidecar then downloads that repo.
#[cfg(target_os = "macos")]
fn seed_fluid_models(models_dir: Option<&Path>, home_dir: Option<&Path>) {
    let Some(src) = models_dir else {
        return; // headless dev: FluidAudio downloads to its own cache as before
    };
    if !src.is_dir() {
        return; // no bundled models (e.g. a dev build packaged without them)
    }
    let Some(home) = home_dir else {
        tracing::warn!("HOME unset; cannot seed FluidAudio models from the bundle");
        return;
    };
    // FluidAudio's default cache: ~/Library/Application Support/FluidAudio/Models/<repo>/.
    let dest = home.join("Library/Application Support/FluidAudio/Models");
    seed_models_into(src, &dest);
}

/// Copy each model-repo subdirectory of `src` into `dest`, skipping any already present there (so it
/// only does work on the first launch / after an erase). Each repo is copied into a hidden `.partial`
/// dir and then atomically renamed into place, so an interrupted copy never leaves a half-tree the
/// sidecar would try to load. Best-effort: a failure is logged, not fatal — the sidecar then
/// downloads that repo. Split from `seed_fluid_models` (which resolves the paths from the env) so
/// the copy logic is unit-testable against temp dirs (the tests run on every platform; the caller
/// is macOS-only).
#[cfg(any(target_os = "macos", test))]
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
/// `seed_fluid_models`; the model repos contain only files and subdirectories (no symlinks).
#[cfg(any(target_os = "macos", test))]
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
