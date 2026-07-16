//! Meetings REST router. Port of `src/hearsay/api/meetings.py`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path, Query};
use crate::routes::Pagination;
use crate::schema::{MeetingCreate, MeetingRead, MeetingUpdate, Page, SegmentRead, StatusInfo};
use crate::state::AppState;
use hearsay_engine::LiveError;

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
        .route("/meetings/{id}/stop", post(stop_meeting))
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
