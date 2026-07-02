//! Hearsay application binary: resolve settings, open the database, bind the loopback listener, and
//! serve the API + UI. `--dump-openapi` prints the OpenAPI document and exits (for the TS codegen);
//! `--synthetic` runs the capture helper in synthetic mode (generated audio, no TCC prompts).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::TcpListener;
use utoipa::OpenApi as _;
use uuid::Uuid;

use hearsay_capture::SwiftHelperSource;
use hearsay_core::{create_app, ApiDoc, AppState, Settings};
use hearsay_orchestrator::{Backend, BackendInstance, Orchestrator, ProcessTranscriber};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The macOS live-capture backend: the Swift `hearsay-helper` for capture + the built `hearsay-live`
/// (Them: diarization + ASR) and `hearsay-me` (Me: VAD + ASR) FluidAudio sidecars, spawned per
/// meeting. Reuses the proven Swift stack behind the orchestrator's traits.
struct MacBackend {
    helper_path: PathBuf,
    synthetic: bool,
}

impl Backend for MacBackend {
    fn build(&self) -> BackendInstance {
        BackendInstance {
            source: Box::new(
                SwiftHelperSource::new(self.helper_path.clone()).synthetic(self.synthetic),
            ),
            me: Box::new(ProcessTranscriber::new(
                self.helper_path.with_file_name("hearsay-me"),
            )),
            them: Box::new(ProcessTranscriber::new(
                self.helper_path.with_file_name("hearsay-live"),
            )),
        }
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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hearsay_core=info,tower_http=info".into()),
        )
        .init();

    let settings = Settings::from_env();
    let pool = hearsay_db::connect(&settings.database_url).await?;
    // 256-bit URL-safe session token (two v4 UUIDs, hex).
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());

    let bind = format!("{}:{}", settings.server_host, settings.server_port);
    let backend = Arc::new(MacBackend {
        helper_path: settings.helper_path.clone(),
        synthetic,
    });
    let engine = Arc::new(Orchestrator::new(
        pool.clone(),
        settings.output_dir.clone(),
        backend,
    ));
    let state = AppState::new(pool, settings, token.clone(), engine);
    let app = create_app(state);

    let listener = TcpListener::bind(&bind).await?;
    let addr = listener.local_addr()?;
    println!("open: http://{addr}/?token={token}");
    tracing::info!(%addr, synthetic, "hearsay core listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
