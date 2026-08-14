//! Speakers (diarization clusters) + identities.

use axum::extract::State;
use axum::routing::{get, put};
use axum::Router;
use uuid::Uuid;

use hearsay_db::queries;
use hearsay_engine::LiveError;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Path, Query};
use crate::routes::{reexport, validated_name, Pagination};
use crate::schema::{IdentityRead, Page, SpeakerMerge, SpeakerRead, SpeakerRename};
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/meetings/{id}/speakers", get(list_speakers))
        .route("/meetings/{id}/speakers/{cluster_id}", put(rename_speaker))
        .route(
            "/meetings/{id}/speakers/{cluster_id}/merge",
            axum::routing::post(merge_speakers),
        )
        .route("/meetings/{id}/rediarize", axum::routing::post(rediarize))
        .route("/identities", get(list_identities))
}

/// The speakers list is unpaginated (few per meeting); the envelope reports the full set.
fn speaker_page(items: Vec<SpeakerRead>) -> Page<SpeakerRead> {
    // The speakers list is unpaginated: one page holding the full set, so page_size mirrors total
    // rather than a windowed limit the caller could page against.
    let total = items.len() as i64;
    Page {
        total,
        page: 1,
        page_size: total as u32,
        items,
    }
}

#[utoipa::path(
    get, path = "/api/meetings/{id}/speakers", tag = "speakers",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Page<SpeakerRead>)),
)]
pub(crate) async fn list_speakers(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Page<SpeakerRead>>> {
    let rows = queries::list_speaker_rows(&state.pool, id).await?;
    let items = rows.into_iter().map(SpeakerRead::from).collect();
    Ok(Json(speaker_page(items)))
}

#[utoipa::path(
    put, path = "/api/meetings/{id}/speakers/{cluster_id}", tag = "speakers",
    params(("id" = Uuid, Path), ("cluster_id" = Uuid, Path)),
    request_body = SpeakerRename,
    responses((status = 200, body = SpeakerRead), (status = 404), (status = 422)),
)]
pub(crate) async fn rename_speaker(
    State(state): State<AppState>,
    Path((meeting_id, cluster_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SpeakerRename>,
) -> ApiResult<Json<SpeakerRead>> {
    let name = validated_name(&body.display_name, "display_name")?;
    let row = queries::rename_cluster(&state.pool, meeting_id, cluster_id, name)
        .await?
        .ok_or(ApiError::NotFound("speaker not found"))?;
    // Keep transcript.md in step with the new label (best-effort; the DB is the source of truth).
    reexport(&state, meeting_id, "speaker rename").await;
    Ok(Json(row.into()))
}

#[utoipa::path(
    post, path = "/api/meetings/{id}/speakers/{cluster_id}/merge", tag = "speakers",
    params(("id" = Uuid, Path), ("cluster_id" = Uuid, Path)),
    request_body = SpeakerMerge,
    responses((status = 200, body = Page<SpeakerRead>), (status = 404), (status = 409), (status = 422)),
)]
pub(crate) async fn merge_speakers(
    State(state): State<AppState>,
    Path((meeting_id, cluster_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SpeakerMerge>,
) -> ApiResult<Json<Page<SpeakerRead>>> {
    // Merging is finalized-only, like reassigning: while recording, the live pipeline is still
    // creating clusters and writing segments underneath us.
    if state.engine.active_meeting() == Some(meeting_id) {
        return Err(ApiError::Conflict(
            "cannot merge speakers while the meeting is recording".into(),
        ));
    }
    match queries::merge_clusters(&state.pool, meeting_id, cluster_id, body.into).await? {
        queries::MergeOutcome::Merged => {}
        queries::MergeOutcome::SourceNotFound => {
            return Err(ApiError::NotFound("speaker not found"))
        }
        // The target comes from the body, so a bad one is invalid input rather than a missing
        // resource — matching how a bad reassign target is reported.
        queries::MergeOutcome::TargetNotFound => {
            return Err(ApiError::Unprocessable(
                "into must be another speaker in this meeting".into(),
            ))
        }
        queries::MergeOutcome::SameCluster => {
            return Err(ApiError::Unprocessable(
                "cannot merge a speaker into itself".into(),
            ))
        }
    }
    reexport(&state, meeting_id, "speaker merge").await;
    let speakers = queries::list_speaker_rows(&state.pool, meeting_id).await?;
    Ok(Json(speaker_page(
        speakers.into_iter().map(SpeakerRead::from).collect(),
    )))
}

#[utoipa::path(
    post, path = "/api/meetings/{id}/rediarize", tag = "speakers",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Page<SpeakerRead>), (status = 404), (status = 409), (status = 503)),
)]
pub(crate) async fn rediarize(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Page<SpeakerRead>>> {
    // Never re-diarize the live meeting: its WAV is still being written, so the refine would read a
    // partial file. The client must stop it first.
    if state.engine.active_meeting() == Some(id) {
        return Err(ApiError::Conflict(
            "cannot re-diarize a meeting while it is recording".into(),
        ));
    }
    // Existence check here so an unknown meeting is a 404 even against a `DisabledEngine` (which
    // would otherwise answer every id with 503). The refine itself is the engine's job — one shared
    // path with the auto-refine at stop, so manual and automatic re-diarization never drift.
    queries::get_meeting(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    match state.engine.rediarize(id).await {
        Ok(()) => {}
        Err(LiveError::Unavailable) => {
            return Err(ApiError::Unavailable(
                "re-diarize unavailable: no recorded audio, or the live engine is not wired".into(),
            ))
        }
        Err(LiveError::Busy(msg)) => return Err(ApiError::Conflict(msg)),
        Err(LiveError::Internal(msg)) => return Err(ApiError::Internal(msg)),
    }
    let speakers = queries::list_speaker_rows(&state.pool, id).await?;
    Ok(Json(speaker_page(
        speakers.into_iter().map(SpeakerRead::from).collect(),
    )))
}

#[utoipa::path(
    get, path = "/api/identities", tag = "speakers",
    params(("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query)),
    responses((status = 200, body = Page<IdentityRead>)),
)]
pub(crate) async fn list_identities(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<Page<IdentityRead>>> {
    let window = pagination.resolve(50, 200);
    let total = queries::count_identities(&state.pool).await?;
    let rows = queries::list_identities(&state.pool, window.limit, window.offset).await?;
    Ok(Json(window.page_of(total, rows)))
}
