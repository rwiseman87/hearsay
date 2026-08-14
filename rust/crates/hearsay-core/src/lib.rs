//! Hearsay core: the axum HTTP + WebSocket API bound to 127.0.0.1 with a per-session bearer token
//! (Host/Origin allowlist). Wires `hearsay-db` and the [`LiveEngine`] seam (implemented later by
//! `hearsay-orchestrator`), serves the React UI bundle, and exposes an OpenAPI document for the
//! TypeScript codegen.
//!
//! The application entrypoint. See
//! `docs/architecture.md`.

pub mod config;
pub mod error;
pub mod extract;
pub mod models;
pub mod openapi;
pub mod routes;
pub mod schema;
pub mod security;
pub mod state;

use axum::body::Body;
use axum::http::header::{REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS};
use axum::http::{HeaderValue, Request, Uri};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::get;
use axum::{Json, Router};
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi as _;
#[cfg(feature = "api-console")]
use utoipa_swagger_ui::SwaggerUi;

pub use config::Settings;
pub use hearsay_engine::{DisabledEngine, LiveEngine, LiveError};
pub use openapi::ApiDoc;
pub use state::AppState;

/// Assemble the full application router (state already provided).
///
/// The `/api` REST routes are gated by the bearer token; `audio` (query token) and the `/ws`
/// WebSocket (query token) authenticate inline. A global layer enforces the loopback Host/Origin
/// allowlist. The UI is mounted only when it has been built.
///
/// Cross-cutting HTTP hygiene wraps everything (outermost first): assign/honor an `X-Request-Id`,
/// emit one access log per request, echo the id back, then stamp the security response headers on
/// every response — including the loopback rejections `enforce_loopback` short-circuits.
pub fn create_app(state: AppState) -> Router {
    let web_dir = state.settings.web_dir.clone();

    let protected = routes::meetings::router()
        .merge(routes::folders::router())
        .merge(routes::speakers::router())
        .merge(routes::notes::router())
        .merge(routes::user_notes::router())
        .merge(routes::models::router())
        .merge(routes::search::router())
        .merge(routes::settings::router())
        .merge(routes::voiceprints::router())
        .route_layer(from_fn_with_state(state.clone(), routes::require_token));
    let api = protected.merge(routes::audio::router());

    // One ServiceBuilder = one layer stack; the first `.layer` is the outermost. `SetRequestId`
    // must precede `TraceLayer` so the span can read the id; `PropagateRequestId` copies it onto the
    // response; the three `SetResponseHeader` layers stamp the fixed security headers last so they
    // land on every response. The per-request CSP + nonce stays on the index handler (it is
    // per-request; see `routes::web`).
    let hygiene = ServiceBuilder::new()
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<Body>| {
                    // `SetRequestId` (outer) has already set the header, so the id is available here.
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        path = %redact_token(request.uri()),
                        request_id,
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ));

    #[cfg_attr(not(feature = "api-console"), allow(unused_mut))]
    let mut app = Router::new()
        .nest("/api", api)
        .merge(routes::ws::router())
        .merge(routes::web::router(&web_dir))
        .route("/openapi.json", get(openapi_json));

    // Browsable API console, behind the `api-console` feature AND ENVIRONMENT=development, so a
    // shipping build neither carries the embedded assets nor mounts the route. It sits outside the
    // `/api` nest, so `enforce_loopback` still gates it but `require_token` does not — the console
    // itself is static, and its "try it out" calls into `/api` carry the token the user pastes into
    // Authorize.
    //
    // The console serves the document from its own path: `SwaggerUi` registers a route for whatever
    // URL it is given, and reusing `/openapi.json` panics the router at startup on the duplicate.
    #[cfg(feature = "api-console")]
    if state.settings.environment == "development" {
        app = app.merge(SwaggerUi::new("/docs").url("/docs/openapi.json", ApiDoc::openapi()));
    }

    app.layer(from_fn(routes::enforce_loopback))
        .layer(hygiene)
        .with_state(state)
}

/// Build a log-safe request target: the path plus its query with any `token` parameter value
/// redacted. The per-session bearer token rides on `?token=` for the SPA navigation, the `<audio>`
/// element, and the WebSocket (channels that cannot set an `Authorization` header), so it must never
/// reach the access log.
fn redact_token(uri: &Uri) -> String {
    match uri.query() {
        None => uri.path().to_string(),
        Some(query) => {
            let redacted = query
                .split('&')
                .map(|pair| {
                    if pair == "token" || pair.starts_with("token=") {
                        "token=REDACTED"
                    } else {
                        pair
                    }
                })
                .collect::<Vec<_>>()
                .join("&");
            format!("{}?{}", uri.path(), redacted)
        }
    }
}

async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::redact_token;
    use axum::http::Uri;

    #[test]
    fn redact_token_scrubs_the_bearer_from_query_uris() {
        // The three token-bearing channels (WS, audio, SPA nav) plus a mixed query.
        let ws: Uri = "/ws?token=deadbeefsecret".parse().unwrap();
        assert_eq!(redact_token(&ws), "/ws?token=REDACTED");

        let audio: Uri = "/api/meetings/abc/audio?token=deadbeefsecret"
            .parse()
            .unwrap();
        assert_eq!(
            redact_token(&audio),
            "/api/meetings/abc/audio?token=REDACTED"
        );

        let mixed: Uri = "/x?page=2&token=deadbeefsecret&page_size=50"
            .parse()
            .unwrap();
        assert_eq!(
            redact_token(&mixed),
            "/x?page=2&token=REDACTED&page_size=50"
        );
    }

    #[test]
    fn redact_token_leaves_non_token_queries_intact() {
        let none: Uri = "/api/meetings".parse().unwrap();
        assert_eq!(redact_token(&none), "/api/meetings");

        let paged: Uri = "/api/meetings?page=1&page_size=25".parse().unwrap();
        assert_eq!(redact_token(&paged), "/api/meetings?page=1&page_size=25");

        // A parameter that merely contains "token" as a substring is not the bearer.
        let similar: Uri = "/x?csrf_token_id=keep".parse().unwrap();
        assert_eq!(redact_token(&similar), "/x?csrf_token_id=keep");
    }
}
