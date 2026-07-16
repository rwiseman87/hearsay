//! Meeting notes (local-LLM summary + action items). `POST /meetings/{id}/notes` generates (or
//! regenerates) them from the finalized transcript; `GET /meetings/{id}/notes` reads the stored row.
//! Thin, like the other routers: existence/state checks here, the LLM work behind the engine seam
//! (`LiveEngine::generate_notes`) — the same path the auto-at-stop generation uses.

use axum::extract::State;
use axum::routing::post;
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;
use hearsay_engine::LiveError;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::schema::MeetingNotesRead;
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new().route("/meetings/{id}/notes", post(generate_notes).get(read_notes))
}

#[utoipa::path(
    post, path = "/api/meetings/{id}/notes", tag = "notes",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = MeetingNotesRead), (status = 404), (status = 409), (status = 422), (status = 503)),
)]
pub(crate) async fn generate_notes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MeetingNotesRead>> {
    // Never summarize the live meeting: its transcript is still being written. Stop it first.
    if state.engine.active_meeting() == Some(id) {
        return Err(ApiError::Conflict(
            "cannot generate notes for a meeting while it is recording".into(),
        ));
    }
    // Existence check here so an unknown meeting is a 404 even against a `DisabledEngine` (which
    // would otherwise answer every id with 503). Generation itself is the engine's job — one shared
    // path with the auto-at-stop generation, so manual and automatic notes never drift.
    queries::get_meeting(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    match state.engine.generate_notes(id).await {
        Ok(()) => {}
        Err(LiveError::Unavailable) => {
            return Err(ApiError::Unavailable(
                "notes unavailable: no summarization model configured, or the engine is not wired"
                    .into(),
            ))
        }
        Err(LiveError::Busy(msg)) => return Err(ApiError::Conflict(msg)),
        Err(LiveError::Internal(msg)) => return Err(ApiError::Internal(msg)),
    }
    // A meeting with no transcript is a no-op generation (nothing persisted); surface that to the
    // client rather than returning an empty 200.
    let notes = queries::get_meeting_notes(&state.pool, id)
        .await?
        .ok_or(ApiError::Unprocessable("no transcript to summarize".into()))?;
    Ok(Json(notes.into()))
}

#[utoipa::path(
    get, path = "/api/meetings/{id}/notes", tag = "notes",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = MeetingNotesRead), (status = 404)),
)]
pub(crate) async fn read_notes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MeetingNotesRead>> {
    let notes = queries::get_meeting_notes(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("no notes for this meeting"))?;
    Ok(Json(notes.into()))
}
