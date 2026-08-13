//! Stored voiceprints: the roster of people the recognizer knows, and the ways to correct it.
//!
//! A voiceprint is not a row of its own — it is the embedding on a meeting's cluster
//! (`clusters.centroid`). Removing one therefore clears that column rather than deleting anything:
//! the cluster, its name binding, and every past transcript survive, and only cross-meeting
//! recognition stops.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, patch};
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path, Query};
use crate::routes::Pagination;
use crate::schema::{IdentityRead, IdentityRename, Page, VoiceprintRead};
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/voiceprints", get(list_voiceprints))
        .route("/voiceprints/{cluster_id}", delete(delete_voiceprint))
        .route("/identities/{id}", patch(rename_identity))
        .route("/identities/{id}/voiceprint", delete(forget_voice))
}

#[utoipa::path(
    get, path = "/api/voiceprints", tag = "voiceprints",
    params(("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query)),
    responses((status = 200, body = Page<VoiceprintRead>)),
)]
pub(crate) async fn list_voiceprints(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<Page<VoiceprintRead>>> {
    let window = pagination.resolve(50, 200);
    // Counts people with a voiceprint, not identities: someone merely named in a meeting that was
    // never refined has no embedding, so they are not part of this roster.
    let total = queries::count_voiceprint_people(&state.pool).await?;
    let people = queries::list_voiceprints(&state.pool, window.limit, window.offset).await?;
    Ok(Json(Page {
        total,
        page: window.page,
        page_size: window.page_size,
        items: people.into_iter().map(VoiceprintRead::from).collect(),
    }))
}

#[utoipa::path(
    patch, path = "/api/identities/{id}", tag = "voiceprints",
    params(("id" = Uuid, Path)),
    request_body = IdentityRename,
    responses((status = 200, body = IdentityRead), (status = 404), (status = 409), (status = 422)),
)]
pub(crate) async fn rename_identity(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<IdentityRename>,
) -> ApiResult<Json<IdentityRead>> {
    let name = body.display_name.trim();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(ApiError::Unprocessable(
            "display_name must be 1..=255 characters".into(),
        ));
    }
    let (identity, meeting_ids) = match queries::rename_identity(&state.pool, id, name).await? {
        queries::RenameIdentityOutcome::Renamed {
            identity,
            meeting_ids,
        } => (identity, meeting_ids),
        queries::RenameIdentityOutcome::NotFound => {
            return Err(ApiError::NotFound("person not found"))
        }
        queries::RenameIdentityOutcome::NameTaken => {
            return Err(ApiError::Conflict(
                "another person already uses that name".into(),
            ))
        }
    };
    // This rename relabelled segments across every meeting the person appears in, so each of those
    // transcripts needs re-rendering. Sequential and best-effort: the writes are cheap (a read plus
    // one atomic file write each) and SQLite is single-writer, so fanning out would only queue.
    for meeting_id in meeting_ids {
        if let Err(err) = state.engine.export_meeting(meeting_id).await {
            tracing::warn!(error = ?err, meeting_id = %meeting_id, "identity rename: re-export failed");
        }
    }
    Ok(Json(identity.into()))
}

#[utoipa::path(
    delete, path = "/api/voiceprints/{cluster_id}", tag = "voiceprints",
    params(("cluster_id" = Uuid, Path)),
    responses((status = 204), (status = 404)),
)]
pub(crate) async fn delete_voiceprint(
    State(state): State<AppState>,
    Path(cluster_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if !queries::clear_cluster_centroid(&state.pool, cluster_id).await? {
        return Err(ApiError::NotFound("voiceprint not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/identities/{id}/voiceprint", tag = "voiceprints",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = 404)),
)]
pub(crate) async fn forget_voice(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if !queries::forget_identity_voice(&state.pool, id).await? {
        return Err(ApiError::NotFound("person not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
