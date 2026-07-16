//! Serve the built web UI (`web/dist`) over the loopback API. Port of `src/hearsay/api/web.py`.
//!
//! The bundle is optional: if it has not been built, no routes are mounted and the API still runs
//! (used by tests and headless `serve`). When present, `GET /` returns `index.html` with the
//! per-session token injected as a global plus a minimal CSP; Vite's hashed assets are served from
//! `/assets`.

use std::path::Path as FsPath;

use axum::extract::{RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;
use uuid::Uuid;

use crate::security::{query_token, token_matches};
use crate::state::AppState;

/// Mount the UI if it is built, else return an empty router (API-only).
pub fn router(web_dir: &FsPath) -> Router<AppState> {
    if !web_dir.join("index.html").is_file() {
        return Router::new();
    }
    let mut router = Router::new().route("/", get(index));
    let assets_dir = web_dir.join("assets");
    if assets_dir.is_dir() {
        router = router.nest_service("/assets", ServeDir::new(assets_dir));
    }
    router
}

/// Minimal CSP for a loopback single-page app: same-origin scripts plus the one nonce'd inline
/// token bootstrap; `connect-src` also allows the loopback WebSocket and — so the Tauri desktop
/// shell's `invoke()` works when it navigates the webview to this served page — the Tauri IPC
/// transport (`ipc://localhost` on macOS, `http://ipc.localhost` on Windows/Linux). Because the core
/// (not Tauri) serves this page, Tauri cannot auto-patch its own CSP, so these must be listed
/// explicitly; they are inert in a plain browser. The WebSocket source is pinned to the exact host
/// the page was served from (the same `window.location.host` the client opens the socket on) rather
/// than a `ws://127.0.0.1:*` port wildcard.
fn csp(nonce: &str, ws_host: &str) -> String {
    [
        "default-src 'self'".to_string(),
        format!("script-src 'self' 'nonce-{nonce}'"),
        "style-src 'self' 'unsafe-inline'".to_string(),
        "img-src 'self' data:".to_string(),
        format!("connect-src 'self' ipc: http://ipc.localhost ws://{ws_host}"),
        "base-uri 'none'".to_string(),
        "object-src 'none'".to_string(),
        "frame-ancestors 'none'".to_string(),
    ]
    .join("; ")
}

/// Inject the token bootstrap `<script>` just before `</head>` (or prepend if there is no head).
fn render_index(template: &str, token: &str, nonce: &str) -> String {
    // serde_json yields a valid JS string literal; the token is hex, so no escaping is needed, but
    // encoding it defensively keeps the bootstrap valid for any token shape.
    let literal = serde_json::to_string(token).unwrap_or_else(|_| "\"\"".to_string());
    let bootstrap =
        format!("<script nonce=\"{nonce}\">window.__HEARSAY_TOKEN__={literal};</script>");
    match template.find("</head>") {
        Some(idx) => format!("{}{bootstrap}{}", &template[..idx], &template[idx..]),
        None => format!("{bootstrap}{template}"),
    }
}

async fn index(
    State(state): State<AppState>,
    req_headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    // Require a valid `?token=` before serving the bootstrap: `GET /` injects the session token as a
    // global, so an unauthenticated request could otherwise harvest it with one curl. Both real
    // clients navigate with `?token=` (the shell handshake; `web/src/api/token.ts`). A browser
    // cannot set an Authorization header on a navigation, so the query param is the only channel.
    if !token_matches(query_token(query.as_deref()), state.session_token.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let index_path = state.settings.web_dir.join("index.html");
    let Ok(template) = tokio::fs::read_to_string(&index_path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let nonce = Uuid::new_v4().simple().to_string();
    let html = render_index(&template, state.session_token.as_str(), &nonce);

    // The Host header is loopback-validated by `enforce_loopback`; pin the WS source to it.
    let ws_host = req_headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1");

    // Only the per-request CSP + nonce lives here: the nonce is minted per response, so it cannot
    // move into the global response-header layer. `X-Content-Type-Options`, `X-Frame-Options`, and
    // `Referrer-Policy` are applied to every response by that layer in `create_app`.
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&csp(&nonce, ws_host)) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    (headers, Html(html)).into_response()
}
