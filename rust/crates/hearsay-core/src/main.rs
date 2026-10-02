//! Hearsay application binary: resolve settings, open the database, bind the loopback listener, and
//! serve the API + UI. `--dump-openapi` prints the OpenAPI document and exits (for the TS codegen);
//! `--synthetic` runs the capture helper in synthetic mode (generated audio, no TCC prompts).

use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::Arc;

use tokio::io::AsyncReadExt as _;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
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
    // Whether this launch has its models, which gates pre-warming (see `LiveEngine::start_prewarm`).
    let models_present = hearsay_core::setup::models_present(&settings);
    if !models_present {
        tracing::info!("models missing: first-run setup required before recording");
    }
    // Assemble the backend behind the LiveEngine seam; the binary holds only the trait object.
    // Config defaults seed it; the editable Settings panels override per meeting.
    let engine_config = EngineConfig {
        pool: pool.clone(),
        output_dir: settings.output_dir.clone(),
        helper_path: settings.helper_path.clone(),
        synthetic,
        prewarm: models_present,
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
        notes_binary: settings.notes_binary.clone(),
    };
    // Dev-only: `HEARSAY_SCRIPTED` swaps the real platform backend for a deterministic, model-free
    // engine that replays a canned meeting (see `hearsay_backends::build_scripted_engine`), so the
    // browser end-to-end test can drive this binary with no capture devices or ANE/GPU. Gated to
    // development so a shipping build never honors it.
    #[cfg(feature = "scripted")]
    let engine = if settings.scripted {
        tracing::info!("HEARSAY_SCRIPTED set: using the scripted (model-free) engine");
        hearsay_backends::build_scripted_engine(engine_config)
    } else {
        build_engine(engine_config)
    };
    #[cfg(not(feature = "scripted"))]
    let engine = build_engine(engine_config);
    // A prior hard exit (SIGKILL / panic / power loss) can strand a meeting row `recording` or
    // `refining` forever, with no session to finalize it. Nothing is active at startup, so sweep and
    // finalize every such row (writing its transcript from the persisted segments) before we serve.
    hearsay_backends::reconcile::reconcile_stranded_meetings(&pool, &settings.output_dir).await;

    // Archive finalized meetings' audio as lossless FLAC once it is old enough, reclaiming ~3x on
    // recordings that would otherwise grow without bound. Periodic rather than one-shot: it must
    // catch meetings that age past the threshold while the app stays open, and it re-reads the
    // setting each tick. Holds a weak engine ref so it stops with the engine, and skips entirely
    // while a meeting is recording.
    // One sweeper shared by the ticker and the Settings "Compress now" button, so they cannot run
    // two passes over the same folders and the UI can watch either one.
    let archive = Arc::new(hearsay_backends::archive::Sweeper::new());
    hearsay_backends::archive::spawn_archive_ticker(
        pool.clone(),
        settings.output_dir.clone(),
        Arc::downgrade(&engine),
        archive.clone(),
        settings.compress_audio,
        settings.compress_after_days,
    );

    // Grab the handshake path before `settings` moves into the app state; the handshake file is
    // written after the listener binds (it carries the resolved port).
    let handshake_path = settings.handshake_path.clone();
    let state = AppState::new(pool, settings, token.clone(), engine.clone(), archive);
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
/// temp file + rename (0600) so the shell never reads a half-written payload. A no-op when
/// the path is `None` (headless dev).
fn write_handshake(port: u16, token: &str, handshake_path: Option<&Path>) -> std::io::Result<()> {
    let Some(path) = handshake_path else {
        return Ok(());
    };
    let payload = serde_json::json!({ "port": port, "token": token }).to_string();

    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    options.mode(0o600);
    {
        use std::io::Write as _;
        let mut file = options.open(&tmp)?;
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Resolve when any shutdown trigger fires: Ctrl-C, SIGTERM, or EOF on our stdin. The desktop
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

    let terminate = async {
        match signal(SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // If we cannot install the handler, never fire this arm.
            Err(_) => std::future::pending::<()>().await,
        }
    };

    tokio::select! {
        _ = ctrl_c => tracing::info!("shutdown: ctrl-c"),
        _ = stdin_eof => tracing::info!("shutdown: stdin closed (parent exited)"),
        _ = terminate => tracing::info!("shutdown: SIGTERM"),
    }
}
