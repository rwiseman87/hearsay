//! First-run model setup. Thin, like the other routers — the work lives in
//! [`crate::setup::SetupManager`].

use axum::extract::State;
use axum::routing::get;
use axum::Router;

use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::schema::{SetupRequest, SetupState};
use crate::setup::StartError;
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new().route("/setup", get(setup_status).post(start_setup))
}

/// Whether first-run model setup is still needed, and how a run is progressing.
///
/// The installer ships no models. `required` stays `true` until the live and refine models are on
/// disk, and the UI blocks recording while it is. `steps` carries per-asset progress during a run,
/// and the work a run would do before one starts.
#[utoipa::path(
    get, path = "/api/setup", tag = "setup",
    responses((status = 200, body = SetupState, description = "Setup requirement + per-step progress")),
)]
pub(crate) async fn setup_status(State(state): State<AppState>) -> Json<SetupState> {
    Json(
        state
            .setup
            .status(&state.pool, &state.settings.refine_model)
            .await,
    )
}

/// Start (or retry) first-run model setup.
///
/// Downloads the missing models in the background, plus the notes model named by `notes_model_id`.
/// Satisfied steps are skipped, so a retry after a failure resumes rather than starting over. Poll
/// `GET /api/setup` for progress.
#[utoipa::path(
    post, path = "/api/setup", tag = "setup",
    request_body = SetupRequest,
    responses(
        (status = 200, body = SetupState, description = "Setup has started"),
        (status = 404, description = "Unknown notes model id"),
        (status = 409, description = "Setup is already running"),
    ),
)]
pub(crate) async fn start_setup(
    State(state): State<AppState>,
    Json(req): Json<SetupRequest>,
) -> ApiResult<Json<SetupState>> {
    match state.setup.start(
        state.pool.clone(),
        state.settings.refine_model.clone(),
        req.notes_model_id,
        state.engine.clone(),
    ) {
        Ok(snapshot) => Ok(Json(snapshot)),
        Err(StartError::UnknownModel) => Err(ApiError::NotFound("unknown model id")),
        Err(StartError::Busy) => Err(ApiError::Conflict("setup is already running".into())),
    }
}
