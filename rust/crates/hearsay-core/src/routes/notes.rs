//! Meeting notes (local-LLM Markdown notes). `POST /meetings/{id}/notes` generates (or regenerates)
//! them from the finalized transcript; `GET /meetings/{id}/notes` reads the stored row. Thin, like the
//! other routers: existence/state checks here, the LLM work behind the engine seam
//! (`LiveEngine::generate_notes`) — the same path the auto-at-stop generation uses. The model's reply
//! is stored and returned verbatim; the prompt template dictates its shape.

use axum::extract::State;
use axum::routing::post;
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;
use hearsay_engine::LiveError;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::schema::{MeetingNotesRead, NotesEdit};
use crate::state::AppState;

/// Boundary limit for a manual notes edit (reject an over-large payload as 422, never let it reach the
/// DB). Generous: notes are a short Markdown document, but a user may paste a long transcript recap.
const MAX_CONTENT_LEN: usize = 40_000;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/meetings/{id}/notes",
        post(generate_notes).get(read_notes).patch(edit_notes),
    )
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
    // The notes are "stale" when the transcript changed after they were generated/edited — a later
    // segment edit bumps that segment's `updated_at` past the notes' `updated_at`.
    let stale = queries::latest_segment_update(&state.pool, id)
        .await?
        .is_some_and(|latest| latest > notes.updated_at);
    let mut read = MeetingNotesRead::from(notes);
    read.stale = stale;
    Ok(Json(read))
}

#[utoipa::path(
    patch, path = "/api/meetings/{id}/notes", tag = "notes",
    params(("id" = Uuid, Path)),
    request_body = NotesEdit,
    responses((status = 200, body = MeetingNotesRead), (status = 404), (status = 409), (status = 422)),
)]
pub(crate) async fn edit_notes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<NotesEdit>,
) -> ApiResult<Json<MeetingNotesRead>> {
    // Editing is finalized-only: never touch a meeting whose transcript is still being written.
    if state.engine.active_meeting() == Some(id) {
        return Err(ApiError::Conflict(
            "cannot edit notes for a meeting while it is recording".into(),
        ));
    }
    let content = body.content.trim();
    if content.chars().count() > MAX_CONTENT_LEN {
        return Err(ApiError::Unprocessable(format!(
            "notes exceed {MAX_CONTENT_LEN} characters"
        )));
    }
    let notes = queries::update_meeting_notes(&state.pool, id, content)
        .await?
        .ok_or(ApiError::NotFound("no notes for this meeting"))?;
    // Keep notes.md in step with the edit (best-effort; the DB is the source of truth).
    if let Err(err) = state.engine.export_meeting(id).await {
        tracing::warn!(error = ?err, meeting_id = %id, "notes edit: re-export failed");
    }
    Ok(Json(notes.into()))
}
