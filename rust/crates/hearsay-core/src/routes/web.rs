//! Serve the built web UI (`web/dist`) over the loopback API. Port of `src/hearsay/api/web.py`.
//!
//! The bundle is optional: if it has not been built, no routes are mounted and the API still runs
//! (used by tests and headless `serve`). When present, `GET /` returns `index.html` with the
//! per-session token injected as a global plus a minimal CSP; Vite's hashed assets are served from
//! `/assets`.

use std::path::Path as FsPath;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;
use uuid::Uuid;

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
/// token bootstrap; `connect-src` also allows the loopback WebSocket.
fn csp(nonce: &str) -> String {
    [
        "default-src 'self'".to_string(),
        format!("script-src 'self' 'nonce-{nonce}'"),
        "style-src 'self' 'unsafe-inline'".to_string(),
        "img-src 'self' data:".to_string(),
        "connect-src 'self' ws://127.0.0.1:* ws://localhost:*".to_string(),
        "base-uri 'none'".to_string(),
        "object-src 'none'".to_string(),
        "frame-ancestors 'none'".to_string(),
    ]
    .join("; ")
}

/// Inject the token bootstrap `<script>` just before `</head>` (or prepend if there is no head).
fn render_index(template: &str, token: &str, nonce: &str) -> String {
    // serde_json yields a valid JS string literal; the token is URL-safe base64.
    let literal = serde_json::to_string(token).unwrap_or_else(|_| "\"\"".to_string());
    let bootstrap =
        format!("<script nonce=\"{nonce}\">window.__HEARSAY_TOKEN__={literal};</script>");
    match template.find("</head>") {
        Some(idx) => format!("{}{bootstrap}{}", &template[..idx], &template[idx..]),
        None => format!("{bootstrap}{template}"),
    }
}

async fn index(State(state): State<AppState>) -> Response {
    let index_path = state.settings.web_dir.join("index.html");
    let Ok(template) = tokio::fs::read_to_string(&index_path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let nonce = Uuid::new_v4().simple().to_string();
    let html = render_index(&template, state.session_token.as_str(), &nonce);

    let mut headers = HeaderMap::new();
    let insert = |headers: &mut HeaderMap, name, value: &str| {
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    };
    insert(&mut headers, header::CONTENT_SECURITY_POLICY, &csp(&nonce));
    insert(&mut headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    insert(&mut headers, header::X_FRAME_OPTIONS, "DENY");
    insert(&mut headers, header::REFERRER_POLICY, "no-referrer");
    (headers, Html(html)).into_response()
}
