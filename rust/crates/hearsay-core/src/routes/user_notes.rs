//! User-authored "My notes" for a meeting (the live notes panel), distinct from the LLM notes in
//! `notes.rs`. `GET /meetings/{id}/user-notes` reads the stored body; `PUT` autosaves it. Unlike the
//! LLM notes, these are taken *while recording*, so the handlers do not gate on the active meeting.

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path};
use crate::schema::{UserNotesRead, UserNotesWrite};
use crate::state::AppState;

/// Reject an over-large notes body at the boundary (422) rather than persisting/exporting it.
const MAX_BODY_LEN: usize = 100_000;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/meetings/{id}/user-notes",
        get(read_user_notes).put(save_user_notes),
    )
}

#[utoipa::path(
    get, path = "/api/meetings/{id}/user-notes", tag = "notes",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = UserNotesRead), (status = 404)),
)]
pub(crate) async fn read_user_notes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<UserNotesRead>> {
    // No row yet is the normal empty state (the user has typed nothing); 404 so the panel starts
    // blank rather than erroring, mirroring the LLM-notes read.
    let notes = queries::get_user_notes(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("no notes for this meeting"))?;
    Ok(Json(notes.into()))
}

#[utoipa::path(
    put, path = "/api/meetings/{id}/user-notes", tag = "notes",
    params(("id" = Uuid, Path)),
    request_body = UserNotesWrite,
    responses((status = 200, body = UserNotesRead), (status = 404), (status = 422)),
)]
pub(crate) async fn save_user_notes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UserNotesWrite>,
) -> ApiResult<Json<UserNotesRead>> {
    if body.body.chars().count() > MAX_BODY_LEN {
        return Err(ApiError::Unprocessable(format!(
            "notes exceed {MAX_BODY_LEN} characters"
        )));
    }
    // The meeting must exist so an unknown id is a 404 rather than an FK-violation 500.
    queries::get_meeting(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    let notes = queries::upsert_user_notes(&state.pool, id, &body.body).await?;
    // Keep my-notes.md in step with the save (best-effort; the DB is the source of truth). Safe while
    // recording — it writes only my-notes.md, never the in-progress transcript.
    if let Err(err) = state.engine.export_user_notes(id).await {
        tracing::warn!(error = ?err, meeting_id = %id, "user-notes save: export failed");
    }
    Ok(Json(notes.into()))
}
