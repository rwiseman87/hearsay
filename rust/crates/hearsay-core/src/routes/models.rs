//! The notes-model download manager API: the in-app catalog + a single-at-a-time anonymous,
//! resumable, SHA256-verified download into the models dir. `GET /models/catalog` lists the curated
//! models (annotated with which are installed); `POST /models/download` starts one in the
//! background; `GET /models/download` is the progress snapshot the UI polls. Thin, like the other
//! routers: the download logic lives in [`crate::models::DownloadManager`].

use axum::extract::State;
use axum::routing::get;
use axum::Router;

use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::models::StartError;
use crate::schema::{DownloadRequest, DownloadState, ModelCatalog};
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new().route("/models/catalog", get(catalog)).route(
        "/models/download",
        get(download_status).post(start_download),
    )
}

#[utoipa::path(
    get, path = "/api/models/catalog", tag = "models",
    responses((status = 200, body = ModelCatalog)),
)]
pub(crate) async fn catalog(State(state): State<AppState>) -> Json<ModelCatalog> {
    Json(state.downloads.catalog())
}

#[utoipa::path(
    get, path = "/api/models/download", tag = "models",
    responses((status = 200, body = DownloadState)),
)]
pub(crate) async fn download_status(State(state): State<AppState>) -> Json<DownloadState> {
    Json(state.downloads.status())
}

#[utoipa::path(
    post, path = "/api/models/download", tag = "models",
    request_body = DownloadRequest,
    responses((status = 200, body = DownloadState), (status = 404), (status = 409)),
)]
pub(crate) async fn start_download(
    State(state): State<AppState>,
    Json(req): Json<DownloadRequest>,
) -> ApiResult<Json<DownloadState>> {
    match state.downloads.start(&req.id, state.pool.clone()) {
        Ok(snapshot) => Ok(Json(snapshot)),
        Err(StartError::UnknownModel) => Err(ApiError::NotFound("unknown model id")),
        Err(StartError::Busy) => Err(ApiError::Conflict(
            "a model download is already in progress".into(),
        )),
    }
}
