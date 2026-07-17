//! Meetings REST router.

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, patch, post, put};
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path, Query};
use crate::routes::settings::reveal_in_file_manager;
use crate::routes::Pagination;
use crate::schema::{
    MeetingCreate, MeetingFolderAssign, MeetingRead, MeetingUpdate, Page, SegmentEdit, SegmentRead,
    StatusInfo,
};
use crate::state::AppState;
use hearsay_engine::LiveError;

/// Max length (chars) of an edited segment's text; longer is rejected at the boundary.
const MAX_SEGMENT_TEXT_LEN: usize = 20_000;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/meetings", get(list_meetings).post(start_meeting))
        .route(
            "/meetings/{id}",
            get(get_meeting)
                .patch(update_meeting)
                .delete(delete_meeting),
        )
        .route("/meetings/{id}/segments", get(list_segments))
        .route("/meetings/{id}/segments/{segment_id}", patch(edit_segment))
        .route("/meetings/{id}/stop", post(stop_meeting))
        .route("/meetings/{id}/folder", put(assign_meeting_folder))
        .route("/meetings/{id}/reveal", post(reveal_meeting))
        .route("/status", get(read_status))
}

#[utoipa::path(get, path = "/api/status", tag = "meetings", responses((status = 200, body = StatusInfo)))]
pub(crate) async fn read_status(State(state): State<AppState>) -> Json<StatusInfo> {
    Json(StatusInfo {
        sidecars_ready: state.engine.sidecars_ready(),
    })
}

fn unavailable() -> ApiError {
    ApiError::Unavailable("live capture engine not available (build hearsay-orchestrator)".into())
}

#[utoipa::path(
    get, path = "/api/meetings", tag = "meetings",
    params(("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query)),
    responses((status = 200, body = Page<MeetingRead>)),
)]
pub(crate) async fn list_meetings(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<Page<MeetingRead>>> {
    let window = pagination.resolve(50, 200);
    let total = queries::count_meetings(&state.pool).await?;
    let rows = queries::list_meetings(&state.pool, window.limit, window.offset).await?;
    Ok(Json(Page {
        total,
        page: window.page,
        page_size: window.page_size,
        items: rows.into_iter().map(MeetingRead::from).collect(),
    }))
}

#[utoipa::path(
    post, path = "/api/meetings", tag = "meetings",
    request_body = MeetingCreate,
    responses((status = 201, body = MeetingRead), (status = 409), (status = 422), (status = 503)),
)]
pub(crate) async fn start_meeting(
    State(state): State<AppState>,
    Json(body): Json<MeetingCreate>,
) -> ApiResult<(StatusCode, Json<MeetingRead>)> {
    if let Some(title) = &body.title {
        if title.chars().count() > 255 {
            return Err(ApiError::Unprocessable(
                "title exceeds 255 characters".into(),
            ));
        }
    }
    match state.engine.start_meeting(body.title).await {
        Ok(meeting) => Ok((StatusCode::CREATED, Json(meeting.into()))),
        Err(LiveError::Busy(msg)) => Err(ApiError::Conflict(msg)),
        Err(LiveError::Unavailable) => Err(unavailable()),
        Err(LiveError::Internal(msg)) => Err(ApiError::Internal(msg)),
    }
}

#[utoipa::path(
    get, path = "/api/meetings/{id}", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = MeetingRead), (status = 404)),
)]
pub(crate) async fn get_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MeetingRead>> {
    let meeting = queries::get_meeting(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    Ok(Json(meeting.into()))
}

#[utoipa::path(
    patch, path = "/api/meetings/{id}", tag = "meetings",
    params(("id" = Uuid, Path)),
    request_body = MeetingUpdate,
    responses((status = 200, body = MeetingRead), (status = 404), (status = 422)),
)]
pub(crate) async fn update_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<MeetingUpdate>,
) -> ApiResult<Json<MeetingRead>> {
    let title = body.title.trim();
    if title.is_empty() {
        return Err(ApiError::Unprocessable("title must not be empty".into()));
    }
    if title.chars().count() > 255 {
        return Err(ApiError::Unprocessable(
            "title exceeds 255 characters".into(),
        ));
    }
    let meeting = queries::update_meeting_title(&state.pool, id, title)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    Ok(Json(meeting.into()))
}

#[utoipa::path(
    put, path = "/api/meetings/{id}/folder", tag = "meetings",
    params(("id" = Uuid, Path)),
    request_body = MeetingFolderAssign,
    responses((status = 200, body = MeetingRead), (status = 404), (status = 422)),
)]
pub(crate) async fn assign_meeting_folder(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<MeetingFolderAssign>,
) -> ApiResult<Json<MeetingRead>> {
    // A non-null target must reference an existing folder; `None` un-files the meeting.
    if let Some(folder_id) = body.folder_id {
        if queries::get_folder(&state.pool, folder_id).await?.is_none() {
            return Err(ApiError::Unprocessable("folder not found".into()));
        }
    }
    let meeting = queries::assign_meeting_folder(&state.pool, id, body.folder_id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    Ok(Json(meeting.into()))
}

#[utoipa::path(
    get, path = "/api/meetings/{id}/segments", tag = "meetings",
    params(
        ("id" = Uuid, Path),
        ("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query),
    ),
    responses((status = 200, body = Page<SegmentRead>)),
)]
pub(crate) async fn list_segments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<Page<SegmentRead>>> {
    let window = pagination.resolve(200, 200);
    let total = queries::count_segments(&state.pool, id).await?;
    let rows = queries::list_segments_page(&state.pool, id, window.limit, window.offset).await?;
    Ok(Json(Page {
        total,
        page: window.page,
        page_size: window.page_size,
        items: rows.into_iter().map(SegmentRead::from).collect(),
    }))
}

#[utoipa::path(
    patch, path = "/api/meetings/{id}/segments/{segment_id}", tag = "meetings",
    params(("id" = Uuid, Path), ("segment_id" = Uuid, Path)),
    request_body = SegmentEdit,
    responses((status = 200, body = SegmentRead), (status = 404), (status = 409), (status = 422)),
)]
pub(crate) async fn edit_segment(
    State(state): State<AppState>,
    Path((id, segment_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SegmentEdit>,
) -> ApiResult<Json<SegmentRead>> {
    // Editing is finalized-only: while recording, the live pipeline is still writing segments.
    if state.engine.active_meeting() == Some(id) {
        return Err(ApiError::Conflict(
            "cannot edit a segment while the meeting is recording".into(),
        ));
    }
    let text = body.text.trim();
    if text.is_empty() {
        return Err(ApiError::Unprocessable("text must not be empty".into()));
    }
    if text.chars().count() > MAX_SEGMENT_TEXT_LEN {
        return Err(ApiError::Unprocessable(format!(
            "text exceeds {MAX_SEGMENT_TEXT_LEN} characters"
        )));
    }
    let segment = queries::update_segment_text(&state.pool, id, segment_id, text)
        .await?
        .ok_or(ApiError::NotFound("segment not found"))?;
    // Keep transcript.md in step with the edit (best-effort; the DB is the source of truth).
    if let Err(err) = state.engine.export_meeting(id).await {
        tracing::warn!(error = ?err, meeting_id = %id, "segment edit: re-export failed");
    }
    Ok(Json(segment.into()))
}

#[utoipa::path(
    post, path = "/api/meetings/{id}/stop", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = MeetingRead), (status = 404), (status = 503)),
)]
pub(crate) async fn stop_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MeetingRead>> {
    match state.engine.stop_meeting(id).await {
        Ok(Some(meeting)) => Ok(Json(meeting.into())),
        Ok(None) => Err(ApiError::NotFound("meeting not found")),
        Err(LiveError::Unavailable) => Err(unavailable()),
        Err(LiveError::Busy(msg)) => Err(ApiError::Conflict(msg)),
        Err(LiveError::Internal(msg)) => Err(ApiError::Internal(msg)),
    }
}

#[utoipa::path(
    delete, path = "/api/meetings/{id}", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404), (status = 409)),
)]
pub(crate) async fn delete_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    // Never delete the live meeting: it would pull the recordings folder out from under the running
    // pipeline. The client must stop it first.
    if state.engine.active_meeting() == Some(id) {
        return Err(ApiError::Conflict(
            "cannot delete a meeting while it is recording".into(),
        ));
    }
    let meeting = queries::get_meeting(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    // Delete the DB rows first (segments + clusters cascade), then best-effort remove the on-disk
    // folder — so a filesystem hiccup only logs a warning (never 500s a delete that already removed
    // the rows), and the folder is never pulled before the rows that reference it.
    if !queries::delete_meeting(&state.pool, id).await? {
        return Err(ApiError::NotFound("meeting not found"));
    }
    let folder = meeting.dir_path(&state.settings.output_dir);
    if let Err(err) = tokio::fs::remove_dir_all(&folder).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %err, folder = %folder.display(), "failed to remove meeting folder");
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Open this meeting's recordings folder (audio, transcript, notes, meeting.json) in the OS file
/// manager — the per-meeting analog of the Settings "Reveal data folder" action, reusing the same
/// `open`-in-Finder helper. Runs in the core (a native process in the user's login session), reached
/// over the same-origin HTTP API. Failures surface the reason (not a generic 500) so a broken reveal
/// is diagnosable.
#[utoipa::path(
    post, path = "/api/meetings/{id}/reveal", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404), (status = 503)),
)]
pub(crate) async fn reveal_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let meeting = queries::get_meeting(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    let dir: PathBuf = meeting.dir_path(&state.settings.output_dir);
    tokio::task::spawn_blocking(move || reveal_in_file_manager(&dir))
        .await
        .map_err(|e| ApiError::Internal(format!("reveal task panicked: {e}")))??;
    Ok(StatusCode::NO_CONTENT)
}
