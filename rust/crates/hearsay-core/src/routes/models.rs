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

/// The notes-model catalog, with each entry's installed state.
///
/// A small fixed catalog of GGUF instruct models, plus the directory they install into. Always
/// compiled, independent of whether the notes sidecar is bundled, so the API surface is identical
/// across builds.
#[utoipa::path(
    get, path = "/api/models/catalog", tag = "models",
    responses((status = 200, body = ModelCatalog, description = "The catalog and the models directory")),
)]
pub(crate) async fn catalog(State(state): State<AppState>) -> Json<ModelCatalog> {
    Json(state.downloads.catalog())
}

/// The current model download's status, for polling.
///
/// One of `idle`, `downloading` (with progress), `verifying` (checking the SHA-256), `ready`, or
/// `error`.
#[utoipa::path(
    get, path = "/api/models/download", tag = "models",
    responses((status = 200, body = DownloadState, description = "The current download status")),
)]
pub(crate) async fn download_status(State(state): State<AppState>) -> Json<DownloadState> {
    Json(state.downloads.status())
}

/// Start downloading a catalog model.
///
/// Downloads into the models directory and verifies it by SHA-256 before marking it ready. One
/// download at a time; poll `GET /api/models/download` for progress.
#[utoipa::path(
    post, path = "/api/models/download", tag = "models",
    request_body = DownloadRequest,
    responses(
        (status = 200, body = DownloadState, description = "The download has started"),
        (status = 404, description = "Unknown model id"),
        (status = 409, description = "A download is already running"),
    ),
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
