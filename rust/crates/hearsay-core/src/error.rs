//! API error type. Renders as a `{ "detail": "..." }` JSON envelope with the right status —
//! the core's own axum error shape. DB failures collapse to a 500 (never leak SQL to the client).

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// An error surfaced to an API client.
#[derive(Debug)]
pub enum ApiError {
    /// 400 — malformed input the DB constraints would otherwise reject.
    BadRequest(String),
    /// 401 — missing or invalid bearer token.
    Unauthorized,
    /// 403 — Origin not allowed.
    Forbidden(&'static str),
    /// 404 — resource not found.
    NotFound(&'static str),
    /// 409 — conflicting state (e.g. a session already recording).
    Conflict(String),
    /// 422 — a semantically invalid value (e.g. an output dir that is missing or not writable).
    Unprocessable(String),
    /// 503 — a capability whose backing engine is absent in this build (capture / inference).
    Unavailable(String),
    /// 500 — an unexpected engine failure (e.g. a sidecar spawn error).
    Internal(String),
    /// 500 — an unexpected database error.
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        ApiError::Db(err)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::BadRequest(m) => write!(f, "{m}"),
            ApiError::Unauthorized => write!(f, "invalid or missing bearer token"),
            ApiError::Forbidden(m) => write!(f, "{m}"),
            ApiError::NotFound(m) => write!(f, "{m}"),
            ApiError::Conflict(m) => write!(f, "{m}"),
            ApiError::Unprocessable(m) => write!(f, "{m}"),
            ApiError::Unavailable(m) => write!(f, "{m}"),
            ApiError::Internal(_) => write!(f, "internal error"),
            ApiError::Db(_) => write!(f, "internal error"),
        }
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(ref msg) => {
                tracing::error!(error = %msg, "engine error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ApiError::Db(ref err) => {
                tracing::error!(error = %err, "database error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = Json(json!({ "detail": self.to_string() }));
        if matches!(self, ApiError::Unauthorized) {
            (status, [(header::WWW_AUTHENTICATE, "Bearer")], body).into_response()
        } else {
            (status, body).into_response()
        }
    }
}

/// Convenience alias for handler results.
pub type ApiResult<T> = Result<T, ApiError>;
