//! Hearsay core: the axum HTTP + WebSocket API bound to 127.0.0.1 with a per-session bearer token
//! (Host/Origin allowlist). Wires `hearsay-db` and the [`LiveEngine`] seam (implemented later by
//! `hearsay-orchestrator`), serves the React UI bundle, and exposes an OpenAPI document for the
//! TypeScript codegen.
//!
//! Port of `src/hearsay/api/` and the application entrypoint. See
//! `docs/architecture-cross-platform.md`.

pub mod config;
pub mod error;
pub mod openapi;
pub mod routes;
pub mod schema;
pub mod security;
pub mod state;
pub mod streaming_transcriber;

use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::get;
use axum::{Json, Router};
use utoipa::OpenApi as _;

pub use config::Settings;
pub use hearsay_engine::{DisabledEngine, LiveEngine, LiveError};
pub use openapi::ApiDoc;
pub use state::AppState;
pub use streaming_transcriber::SherpaTranscriber;

/// Assemble the full application router (state already provided).
///
/// The `/api` REST routes are gated by the bearer token; `audio` (query token) and the `/ws`
/// WebSocket (query token) authenticate inline. A global layer enforces the loopback Host/Origin
/// allowlist. The UI is mounted only when it has been built.
pub fn create_app(state: AppState) -> Router {
    let web_dir = state.settings.web_dir.clone();

    let protected = routes::meetings::router()
        .merge(routes::speakers::router())
        .merge(routes::settings::router())
        .route_layer(from_fn_with_state(state.clone(), routes::require_token));
    let api = protected.merge(routes::audio::router());

    Router::new()
        .nest("/api", api)
        .merge(routes::ws::router())
        .merge(routes::web::router(&web_dir))
        .route("/openapi.json", get(openapi_json))
        .layer(from_fn(routes::enforce_loopback))
        .with_state(state)
}

async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
