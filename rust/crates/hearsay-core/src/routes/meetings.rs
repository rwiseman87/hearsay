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
use crate::routes::{reexport, validated_name};
use crate::schema::{
    MeetingCreate, MeetingFolderAssign, MeetingRead, MeetingUpdate, Page, SegmentEdit, SegmentRead,
    SegmentSpeakerAssign, StatusInfo,
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
        .route(
            "/meetings/{id}/segments/{segment_id}/speaker",
            patch(reassign_segment_speaker),
        )
        .route("/meetings/{id}/stop", post(stop_meeting))
        .route("/meetings/{id}/keep-recording", post(keep_recording))
        .route("/meetings/{id}/pause", post(pause_meeting))
        .route("/meetings/{id}/resume", post(resume_meeting))
        .route("/meetings/{id}/folder", put(assign_meeting_folder))
        .route("/meetings/{id}/reveal", post(reveal_meeting))
        .route("/status", get(read_status))
}

/// Live app readiness, for the UI header.
///
/// Reports whether the transcription sidecars have finished loading their models. A cold start
/// takes several seconds, so the record control polls this to show a "preparing" state rather than
/// accepting a start that would stall. Read-only and cheap to poll.
#[utoipa::path(
    get, path = "/api/status", tag = "meetings",
    responses((status = 200, body = StatusInfo, description = "Current sidecar readiness")),
)]
pub(crate) async fn read_status(State(state): State<AppState>) -> Json<StatusInfo> {
    Json(StatusInfo {
        sidecars_ready: state.engine.sidecars_ready(),
    })
}

fn unavailable() -> ApiError {
    ApiError::Unavailable("live capture engine not available (build hearsay-orchestrator)".into())
}

/// List meetings, newest first.
///
/// Backs the Library and Dashboard views.
#[utoipa::path(
    get, path = "/api/meetings", tag = "meetings",
    params(("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query)),
    responses((status = 200, body = Page<MeetingRead>, description = "A page of meetings, newest first")),
)]
pub(crate) async fn list_meetings(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<Page<MeetingRead>>> {
    let window = pagination.resolve(50, 200);
    let total = queries::count_meetings(&state.pool).await?;
    let rows = queries::list_meetings(&state.pool, window.limit, window.offset).await?;
    Ok(Json(window.page_of(total, rows)))
}

/// Start a meeting.
///
/// Spawns the capture helper, begins capture, and starts the transcription pipeline. One meeting
/// may be active at a time. `title` is optional; omitted, it defaults to a timestamp-derived name.
#[utoipa::path(
    post, path = "/api/meetings", tag = "meetings",
    request_body = MeetingCreate,
    responses(
        (status = 201, body = MeetingRead, description = "Recording started"),
        (status = 409, description = "A meeting is already recording"),
        (status = 422, description = "Title exceeds 255 characters"),
        (status = 503, description = "Capture engine unavailable"),
    ),
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

/// Read one meeting.
#[utoipa::path(
    get, path = "/api/meetings/{id}", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = MeetingRead, description = "The meeting"),
        (status = 404, description = "No such meeting"),
    ),
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

/// Rename a meeting.
///
/// Updates the title only; filing a meeting into a folder is a separate operation
/// (`PUT /api/meetings/{id}/folder`). The title is trimmed before it is stored.
#[utoipa::path(
    patch, path = "/api/meetings/{id}", tag = "meetings",
    params(("id" = Uuid, Path)),
    request_body = MeetingUpdate,
    responses(
        (status = 200, body = MeetingRead, description = "The renamed meeting"),
        (status = 404, description = "No such meeting"),
        (status = 422, description = "Blank title, or over 255 characters"),
    ),
)]
pub(crate) async fn update_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<MeetingUpdate>,
) -> ApiResult<Json<MeetingRead>> {
    let title = body.title.trim();
    let title = validated_name(title, "title")?;
    let meeting = queries::update_meeting_title(&state.pool, id, title)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;
    Ok(Json(meeting.into()))
}

/// File a meeting into a folder.
///
/// A `null` `folder_id` un-files the meeting back to the root. Organizational only — the meeting's
/// on-disk directory never moves.
#[utoipa::path(
    put, path = "/api/meetings/{id}/folder", tag = "meetings",
    params(("id" = Uuid, Path)),
    request_body = MeetingFolderAssign,
    responses(
        (status = 200, body = MeetingRead, description = "The re-filed meeting"),
        (status = 404, description = "The meeting or the target folder is unknown"),
        (status = 422, description = "Malformed request body"),
    ),
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

/// List a meeting's finalized transcript segments, ordered by `start_s`.
///
/// How a past meeting reloads from the database, and how a live client recovers after a `resync`
/// event on the transcript WebSocket. Only finalized segments are stored; streamed partials are
/// never persisted.
#[utoipa::path(
    get, path = "/api/meetings/{id}/segments", tag = "meetings",
    params(
        ("id" = Uuid, Path),
        ("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query),
    ),
    responses((status = 200, body = Page<SegmentRead>, description = "A page of segments, ordered by start time")),
)]
pub(crate) async fn list_segments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<Page<SegmentRead>>> {
    let window = pagination.resolve(200, 200);
    let total = queries::count_segments(&state.pool, id).await?;
    let rows = queries::list_segments_page(&state.pool, id, window.limit, window.offset).await?;
    Ok(Json(window.page_of(total, rows)))
}

/// Correct one transcript line's text.
///
/// The database is the source of truth; `transcript.md` is re-exported best-effort afterwards.
/// Finalized meetings only — while recording, the live pipeline is still appending segments.
#[utoipa::path(
    patch, path = "/api/meetings/{id}/segments/{segment_id}", tag = "meetings",
    params(("id" = Uuid, Path), ("segment_id" = Uuid, Path)),
    request_body = SegmentEdit,
    responses(
        (status = 200, body = SegmentRead, description = "The edited segment"),
        (status = 404, description = "The segment is not in this meeting"),
        (status = 409, description = "The meeting is still recording"),
        (status = 422, description = "Empty text, or over the length limit"),
    ),
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
    reexport(&state, id, "segment edit").await;
    Ok(Json(segment.into()))
}

/// Reassign one line to a different speaker.
///
/// Fixes an individual diarization mistake without renaming the whole cluster. Send exactly one of
/// `cluster_id` (move the line to an existing speaker in this meeting) or `display_name` (assign it
/// to a person by name, reusing that identity's cluster in the meeting if it has one, else creating
/// a new locked speaker).
///
/// The line is flagged `edited`, so it counts toward the warning shown before a refine discards
/// manual work. The database is the source of truth; `transcript.md` is re-exported best-effort.
#[utoipa::path(
    patch, path = "/api/meetings/{id}/segments/{segment_id}/speaker", tag = "meetings",
    params(("id" = Uuid, Path), ("segment_id" = Uuid, Path)),
    request_body = SegmentSpeakerAssign,
    responses(
        (status = 200, body = SegmentRead, description = "The reassigned segment"),
        (status = 404, description = "The segment is not in this meeting"),
        (status = 409, description = "The meeting is still recording"),
        (status = 422, description = "A Me line, an unknown target cluster, or a body that is not exactly one of the two fields"),
    ),
)]
pub(crate) async fn reassign_segment_speaker(
    State(state): State<AppState>,
    Path((id, segment_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SegmentSpeakerAssign>,
) -> ApiResult<Json<SegmentRead>> {
    // Reassigning is finalized-only: while recording, the live pipeline is still writing segments.
    if state.engine.active_meeting() == Some(id) {
        return Err(ApiError::Conflict(
            "cannot reassign a segment while the meeting is recording".into(),
        ));
    }
    // Exactly one of cluster_id / display_name selects the target speaker.
    let target = match (body.cluster_id, body.display_name.as_deref()) {
        (Some(cluster_id), None) => queries::SpeakerTarget::Cluster(cluster_id),
        (None, Some(name)) => {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 255 {
                return Err(ApiError::Unprocessable(
                    "display_name must be 1..=255 characters".into(),
                ));
            }
            queries::SpeakerTarget::Name(name)
        }
        _ => {
            return Err(ApiError::Unprocessable(
                "provide exactly one of cluster_id or display_name".into(),
            ))
        }
    };
    let segment =
        match queries::reassign_segment_speaker(&state.pool, id, segment_id, target).await? {
            queries::ReassignOutcome::Reassigned(segment) => segment,
            queries::ReassignOutcome::SegmentNotFound => {
                return Err(ApiError::NotFound("segment not found"))
            }
            queries::ReassignOutcome::NotThemStream => {
                return Err(ApiError::Unprocessable(
                    "only Them lines can be reassigned".into(),
                ))
            }
            queries::ReassignOutcome::ClusterNotFound => {
                return Err(ApiError::Unprocessable(
                    "cluster not found in meeting".into(),
                ))
            }
        };
    // Keep transcript.md in step with the reassignment (best-effort; the DB is the source of truth).
    reexport(&state, id, "segment reassignment").await;
    Ok(Json(segment.into()))
}

/// Stop and finalize a meeting.
///
/// Stops capture, flushes the pipeline, rewrites `transcript.md` in timestamp order, and stamps
/// `ended_at`. Returns the updated meeting immediately: when a refine or notes step will run the
/// status is `refining`, and it flips to `finalized` once that background work completes.
#[utoipa::path(
    post, path = "/api/meetings/{id}/stop", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = MeetingRead, description = "The finalized (or refining) meeting"),
        (status = 404, description = "No such meeting"),
        (status = 503, description = "Capture engine unavailable"),
    ),
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

/// "Keep recording": reset the active meeting's inactivity clock so a present-but-quiet user is not
/// nudged again or auto-ended. A no-op for any meeting that is not the current recording session
/// (404) — there is no clock to reset. Idempotent and cheap; the client calls it when the user
/// dismisses the "still recording?" prompt.
#[utoipa::path(
    post, path = "/api/meetings/{id}/keep-recording", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Silence clock reset"),
        (status = 404, description = "Not the current recording session"),
    ),
)]
pub(crate) async fn keep_recording(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if state.engine.active_meeting() != Some(id) {
        return Err(ApiError::NotFound("meeting is not recording"));
    }
    state.engine.keep_alive(id);
    Ok(StatusCode::NO_CONTENT)
}

/// Pause the live meeting's capture (the "Pause" control): recording + transcription stop and the
/// timeline freezes with no gap until resumed. A 404 for any meeting that is not the current
/// recording session. Idempotent (pausing an already-paused meeting is a no-op 204).
#[utoipa::path(
    post, path = "/api/meetings/{id}/pause", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Paused (or already paused)"),
        (status = 404, description = "Not the current recording session"),
    ),
)]
pub(crate) async fn pause_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if !state.engine.pause_meeting(id) {
        return Err(ApiError::NotFound("meeting is not recording"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Resume a paused meeting's capture. A 404 for any meeting that is not the current recording
/// session. Idempotent (resuming a non-paused meeting is a no-op 204).
#[utoipa::path(
    post, path = "/api/meetings/{id}/resume", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Resumed (or already running)"),
        (status = 404, description = "Not the current recording session"),
    ),
)]
pub(crate) async fn resume_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if !state.engine.resume_meeting(id) {
        return Err(ApiError::NotFound("meeting is not recording"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Delete a meeting and its recordings.
///
/// Removes the database rows (segments, clusters, and both notes tables cascade) and deletes the
/// on-disk folder. Irreversible. The active recording session cannot be deleted — stop it first.
#[utoipa::path(
    delete, path = "/api/meetings/{id}", tag = "meetings",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such meeting"),
        (status = 409, description = "The meeting is currently recording"),
    ),
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
    responses(
        (status = 204, description = "Handed off to the file manager"),
        (status = 404, description = "No such meeting"),
        (status = 503, description = "The folder could not be opened"),
    ),
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
