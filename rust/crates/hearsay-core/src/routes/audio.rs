//! Meeting audio playback: serve the mixed WAV so the UI can replay a meeting.
//!
//! Auth accepts the per-session token either as the `?token=` query param (an `<audio>` element
//! cannot set an Authorization header, so the browser passes it in the URL, mirroring the
//! WebSocket) or as a normal bearer header. `ServeFile` honours Range requests, so the browser can
//! seek. Port of `src/hearsay/api/audio.py`.

use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use hearsay_db::queries;

use crate::security::{bearer_token, token_matches};
use crate::state::AppState;

/// Routes served under the `/api` prefix (auth is inline, not the bearer route layer).
pub fn router() -> Router<AppState> {
    Router::new().route("/meetings/{id}/audio", get(get_meeting_audio))
}

/// Extract the `token` value from a raw query string (`a=b&token=xyz`), without percent-decoding
/// (the session token is URL-safe base64).
fn query_token(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "detail": "invalid or missing token" })),
    )
        .into_response()
}

async fn get_meeting_audio(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Response {
    let token = query_token(request.uri().query()).or_else(|| {
        bearer_token(
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
        )
    });
    if !token_matches(token, state.session_token.as_str()) {
        return unauthorized();
    }

    let meeting = match queries::get_meeting(&state.pool, id).await {
        Ok(Some(meeting)) => meeting,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "detail": "meeting not found" })),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, "database error serving audio");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let audio_path = meeting
        .dir_path(&state.settings.output_dir)
        .join("audio.wav");
    if !audio_path.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "no audio recorded for this meeting" })),
        )
            .into_response();
    }

    match ServeFile::new(audio_path).oneshot(request).await {
        Ok(response) => response.into_response(),
        Err(infallible) => match infallible {},
    }
}
