//! HTTP routers + the loopback-hardening and token middleware. Routers stay thin: validate, call a
//! query / the engine, return.

pub mod audio;
pub mod folders;
pub mod meetings;
pub mod models;
pub mod notes;
pub mod search;
pub mod settings;
pub mod setup;
pub mod speakers;
pub mod user_notes;
pub mod voiceprints;
pub mod web;
pub mod ws;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::schema::Page;
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

impl PageWindow {
    /// Wrap this window's rows in the shared list envelope, converting each row into its API type.
    pub fn page_of<R, T: From<R>>(&self, total: i64, rows: Vec<R>) -> Page<T> {
        Page {
            total,
            page: self.page,
            page_size: self.page_size,
            items: rows.into_iter().map(T::from).collect(),
        }
    }

    /// The empty page for this window, for a query that is a valid no-op rather than an error.
    pub fn empty_page<T>(&self) -> Page<T> {
        Page {
            total: 0,
            page: self.page,
            page_size: self.page_size,
            items: Vec::new(),
        }
    }
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

/// Trim `raw` and require 1..=255 characters, naming `field` in the 422. The shared shape behind
/// every user-supplied name, title, and display name.
pub(crate) fn validated_name<'a>(raw: &'a str, field: &str) -> ApiResult<&'a str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::Unprocessable(format!(
            "{field} must not be empty"
        )));
    }
    if name.chars().count() > 255 {
        return Err(ApiError::Unprocessable(format!(
            "{field} exceeds 255 characters"
        )));
    }
    Ok(name)
}

/// Re-render a meeting's Markdown after a write that changed it, best-effort: the database is the
/// source of truth, so a failed export is logged and the request still succeeds. `what` names the
/// edit in that log line.
pub(crate) async fn reexport(state: &AppState, meeting_id: Uuid, what: &str) {
    if let Err(err) = state.engine.export_meeting(meeting_id).await {
        tracing::warn!(error = ?err, %meeting_id, "{what}: re-export failed");
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
