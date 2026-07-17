//! HTTP routers + the loopback-hardening and token middleware. Routers stay thin: validate, call a
//! query / the engine, return. Port of `src/hearsay/api/`.

pub mod audio;
pub mod folders;
pub mod meetings;
pub mod models;
pub mod notes;
pub mod search;
pub mod settings;
pub mod speakers;
pub mod web;
pub mod ws;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::error::ApiError;
use crate::security::{bearer_token, host_allowed, origin_allowed, token_matches};
use crate::state::AppState;

/// Pagination query parameters shared by the list endpoints.
#[derive(Debug, Default, Deserialize)]
pub struct Pagination {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// A resolved page window (1-based `page`, clamped `page_size`, and the SQL `limit`/`offset`).
pub struct PageWindow {
    pub page: u32,
    pub page_size: u32,
    pub limit: i64,
    pub offset: i64,
}

impl Pagination {
    /// Resolve to a concrete window, applying the per-endpoint default size and 1..=`max_size` bound.
    pub fn resolve(&self, default_size: u32, max_size: u32) -> PageWindow {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(default_size).clamp(1, max_size);
        let offset = i64::from(page - 1) * i64::from(page_size);
        PageWindow {
            page,
            page_size,
            limit: i64::from(page_size),
            offset,
        }
    }
}

/// Global middleware: reject non-loopback `Host` (DNS-rebinding) and cross-site `Origin`.
pub async fn enforce_loopback(request: Request, next: Next) -> Response {
    let headers = request.headers();
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if !host_allowed(host) {
        return ApiError::BadRequest("host not allowed".into()).into_response();
    }
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !origin_allowed(origin) {
        return ApiError::Forbidden("origin not allowed").into_response();
    }
    next.run(request).await
}

/// Route-layer middleware: require a valid bearer token (the gate for the REST API).
pub async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !token_matches(bearer_token(header), state.session_token.as_str()) {
        return ApiError::Unauthorized.into_response();
    }
    next.run(request).await
}
