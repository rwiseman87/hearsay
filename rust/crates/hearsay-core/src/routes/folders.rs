//! Folders REST router: the nested organizational folder tree meetings are filed under. Thin
//! handlers — validate, call a query, map to a DTO. Distinct from `meetings.folder` (a directory
//! name); a meeting's membership is its `folder_id` (see `PUT /api/meetings/{id}/folder`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, patch, put};
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path, Query};
use crate::routes::Pagination;
use crate::schema::{FolderCreate, FolderRead, FolderReparent, FolderUpdate, Page};
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/folders", get(list_folders).post(create_folder))
        .route("/folders/{id}", patch(rename_folder).delete(delete_folder))
        .route("/folders/{id}/parent", put(reparent_folder))
}

/// Trim and length-check a folder name (non-empty, <= 255 chars), mirroring the meeting-title rule.
fn validated_name(raw: &str) -> ApiResult<&str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::Unprocessable("name must not be empty".into()));
    }
    if name.chars().count() > 255 {
        return Err(ApiError::Unprocessable(
            "name exceeds 255 characters".into(),
        ));
    }
    Ok(name)
}

#[utoipa::path(
    get, path = "/api/folders", tag = "folders",
    params(("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query)),
    responses((status = 200, body = Page<FolderRead>)),
)]
pub(crate) async fn list_folders(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<Page<FolderRead>>> {
    // Folders are few and the sidebar builds the tree client-side, so the default page returns the
    // whole set; the bound is a safety cap, not an expected paging boundary.
    let window = pagination.resolve(200, 500);
    let total = queries::count_folders(&state.pool).await?;
    let rows = queries::list_folders(&state.pool, window.limit, window.offset).await?;
    Ok(Json(Page {
        total,
        page: window.page,
        page_size: window.page_size,
        items: rows.into_iter().map(FolderRead::from).collect(),
    }))
}

#[utoipa::path(
    post, path = "/api/folders", tag = "folders",
    request_body = FolderCreate,
    responses((status = 201, body = FolderRead), (status = 422)),
)]
pub(crate) async fn create_folder(
    State(state): State<AppState>,
    Json(body): Json<FolderCreate>,
) -> ApiResult<(StatusCode, Json<FolderRead>)> {
    let name = validated_name(&body.name)?;
    if let Some(parent_id) = body.parent_id {
        if queries::get_folder(&state.pool, parent_id).await?.is_none() {
            return Err(ApiError::Unprocessable("parent folder not found".into()));
        }
    }
    let folder = queries::create_folder(&state.pool, name, body.parent_id).await?;
    Ok((StatusCode::CREATED, Json(folder.into())))
}

#[utoipa::path(
    patch, path = "/api/folders/{id}", tag = "folders",
    params(("id" = Uuid, Path)),
    request_body = FolderUpdate,
    responses((status = 200, body = FolderRead), (status = 404), (status = 422)),
)]
pub(crate) async fn rename_folder(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<FolderUpdate>,
) -> ApiResult<Json<FolderRead>> {
    let name = validated_name(&body.name)?;
    let folder = queries::update_folder_name(&state.pool, id, name)
        .await?
        .ok_or(ApiError::NotFound("folder not found"))?;
    Ok(Json(folder.into()))
}

#[utoipa::path(
    put, path = "/api/folders/{id}/parent", tag = "folders",
    params(("id" = Uuid, Path)),
    request_body = FolderReparent,
    responses((status = 200, body = FolderRead), (status = 404), (status = 422)),
)]
pub(crate) async fn reparent_folder(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<FolderReparent>,
) -> ApiResult<Json<FolderRead>> {
    // The folder being moved must exist (so a missing folder is a 404, not the parent's 422 below).
    if queries::get_folder(&state.pool, id).await?.is_none() {
        return Err(ApiError::NotFound("folder not found"));
    }
    if let Some(parent_id) = body.parent_id {
        if queries::get_folder(&state.pool, parent_id).await?.is_none() {
            return Err(ApiError::Unprocessable("parent folder not found".into()));
        }
        // Reject a move into the folder itself or one of its descendants — that would detach the
        // subtree into a cycle.
        if queries::folder_is_descendant(&state.pool, parent_id, id).await? {
            return Err(ApiError::Unprocessable(
                "cannot move a folder into itself or one of its descendants".into(),
            ));
        }
    }
    let folder = queries::set_folder_parent(&state.pool, id, body.parent_id)
        .await?
        .ok_or(ApiError::NotFound("folder not found"))?;
    Ok(Json(folder.into()))
}

#[utoipa::path(
    delete, path = "/api/folders/{id}", tag = "folders",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404)),
)]
pub(crate) async fn delete_folder(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    // The sub-folder subtree is removed and the meetings within are un-filed (FK cascades); the
    // meetings themselves are never deleted.
    if !queries::delete_folder(&state.pool, id).await? {
        return Err(ApiError::NotFound("folder not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
