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

/// List a meeting's diarized speakers.
///
/// Each item carries the cluster's resolved `label`: a bound name if the speaker has been named,
/// else `Speaker N`. Me is the microphone channel, not a cluster, so it never appears here.
#[utoipa::path(
    get, path = "/api/meetings/{id}/speakers", tag = "speakers",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Page<SpeakerRead>, description = "A page of this meeting's speakers")),
)]
pub(crate) async fn list_speakers(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Page<SpeakerRead>>> {
    let rows = queries::list_speaker_rows(&state.pool, id).await?;
    let items = rows.into_iter().map(SpeakerRead::from).collect();
    Ok(Json(speaker_page(items)))
}

/// Name a speaker, binding the cluster to a cross-meeting identity.
///
/// Gets or creates the identity by name, locks the binding, and relabels that speaker's segments.
/// A locked binding wins over voiceprint recognition and survives a re-diarize. If the meeting is
/// still active the name also propagates to the live pipeline, so subsequent utterances carry it.
#[utoipa::path(
    put, path = "/api/meetings/{id}/speakers/{cluster_id}", tag = "speakers",
    params(("id" = Uuid, Path), ("cluster_id" = Uuid, Path)),
    request_body = SpeakerRename,
    responses(
        (status = 200, body = SpeakerRead, description = "The renamed speaker"),
        (status = 404, description = "The cluster is not in this meeting"),
        (status = 422, description = "Blank name"),
    ),
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

/// Combine two speakers the diarizer split apart.
///
/// Folds the cluster named in the path into the one named by `into`: its lines are repointed,
/// relabelled to the target's name, and flagged `edited`, and the now-empty cluster is deleted.
/// Scoped to this meeting — no other meeting is touched, and the target's stored voiceprint is left
/// alone. The source's own voice sample goes with its row, so a spurious split stops feeding
/// recognition.
///
/// The freed ordinal is not reused, so labels can read "Speaker 1, Speaker 3"; renumbering would
/// rename unrelated speakers whose lines still carry the old text.
#[utoipa::path(
    post, path = "/api/meetings/{id}/speakers/{cluster_id}/merge", tag = "speakers",
    params(("id" = Uuid, Path), ("cluster_id" = Uuid, Path)),
    request_body = SpeakerMerge,
    responses(
        (status = 200, body = Page<SpeakerRead>, description = "The meeting's speakers after the merge"),
        (status = 404, description = "The path cluster is not in this meeting"),
        (status = 409, description = "The meeting is still recording"),
        (status = 422, description = "A self-merge, or an `into` cluster outside this meeting"),
    ),
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

/// Re-run the offline refine ("Refine speakers").
///
/// Re-diarizes and re-transcribes the whole Them track: global clustering, overlap handling, and
/// voiceprint recognition of people named in earlier meetings. More accurate than the streaming
/// labels, which run online with limited context.
///
/// Manual (locked) names are carried across. Segment-level work is not: per-line reassignments and
/// hand-edited text are rebuilt only at the cluster level, which is what the `edited` flag warns
/// about before the user starts a refine.
#[utoipa::path(
    post, path = "/api/meetings/{id}/rediarize", tag = "speakers",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = Page<SpeakerRead>, description = "The meeting's speakers after the refine"),
        (status = 404, description = "No such meeting"),
        (status = 409, description = "The meeting is still recording"),
        (status = 503, description = "The refine model or diarizer sidecar is unavailable"),
    ),
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

/// List known people, most recently updated first.
///
/// Every name that has ever been bound to a speaker, whether or not it has a stored voiceprint.
/// Powers the rename autocomplete, so a name used in one meeting is suggested in the next. For the
/// subset that can actually be recognized by voice, see `GET /api/voiceprints`.
#[utoipa::path(
    get, path = "/api/identities", tag = "speakers",
    params(("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query)),
    responses((status = 200, body = Page<IdentityRead>, description = "A page of known people")),
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
