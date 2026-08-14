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

/// Read the user-authored "My notes" for a meeting.
///
/// Free-form text the user types themselves, distinct from the LLM-generated notes. One row per
/// meeting. A `404` is the normal empty state — the panel starts blank.
#[utoipa::path(
    get, path = "/api/meetings/{id}/user-notes", tag = "notes",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = UserNotesRead, description = "The stored body and its last-saved time"),
        (status = 404, description = "Nothing has been typed for this meeting yet"),
    ),
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

/// Autosave the user-authored "My notes" for a meeting.
///
/// The database is the source of truth; `my-notes.md` in the meeting folder is mirrored
/// best-effort. Unlike the generated notes these may be written while the meeting is still
/// recording, so this route does not gate on the active session.
#[utoipa::path(
    put, path = "/api/meetings/{id}/user-notes", tag = "notes",
    params(("id" = Uuid, Path)),
    request_body = UserNotesWrite,
    responses(
        (status = 200, body = UserNotesRead, description = "The saved notes"),
        (status = 404, description = "No such meeting"),
        (status = 422, description = "Body exceeds 100,000 characters"),
    ),
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
