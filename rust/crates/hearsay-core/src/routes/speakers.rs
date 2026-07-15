//! Speakers (diarization clusters) + identities. Port of `src/hearsay/api/speakers.py`.

use axum::extract::{Path, Query, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use uuid::Uuid;

use hearsay_db::queries;

use crate::error::{ApiError, ApiResult};
use crate::routes::Pagination;
use crate::schema::{IdentityRead, Page, SpeakerRead, SpeakerRename};
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/meetings/{id}/speakers", get(list_speakers))
        .route("/meetings/{id}/speakers/{cluster_id}", put(rename_speaker))
        .route("/meetings/{id}/rediarize", axum::routing::post(rediarize))
        .route("/identities", get(list_identities))
}

/// The speakers list is unpaginated (few per meeting); the envelope reports the full set.
fn speaker_page(items: Vec<SpeakerRead>) -> Page<SpeakerRead> {
    let total = items.len() as i64;
    let page_size = items.len().max(1) as u32;
    Page {
        total,
        page: 1,
        page_size,
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
    Path((_meeting_id, cluster_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SpeakerRename>,
) -> ApiResult<Json<SpeakerRead>> {
    let name = body.display_name.trim();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(ApiError::BadRequest(
            "display_name must be 1..=255 characters".into(),
        ));
    }
    let row = queries::rename_cluster(&state.pool, cluster_id, name)
        .await?
        .ok_or(ApiError::NotFound("speaker not found"))?;
    Ok(Json(row.into()))
}

#[utoipa::path(
    post, path = "/api/meetings/{id}/rediarize", tag = "speakers",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Page<SpeakerRead>), (status = 404), (status = 503)),
)]
pub(crate) async fn rediarize(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Page<SpeakerRead>>> {
    let meeting = queries::get_meeting(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("meeting not found"))?;

    let dir = meeting.dir_path(&state.settings.output_dir);
    let audio = dir.join("audio.wav");
    if !audio.exists() {
        return Err(ApiError::Unavailable(
            "no recorded audio to re-diarize (audio.record was off)".into(),
        ));
    }
    let diarize = state.settings.helper_path.with_file_name("hearsay-diarize");
    if !diarize.exists() {
        return Err(ApiError::Unavailable(
            "hearsay-diarize sidecar not found (build it with `make swift-build`)".into(),
        ));
    }
    // Effective refine model: the stored `models` override else the config default, read fresh so a
    // change in the Models panel applies to the next manual re-diarize (matching the auto-refine).
    let model = queries::effective_refine_model(&state.pool, &state.settings.refine_model).await?;
    let timeout = state.settings.refine_timeout;

    // Read the Them track, re-diarize (hearsay-diarize) + re-transcribe (whisper) — all blocking.
    let refined = tokio::task::spawn_blocking(move || {
        hearsay_inference::refine_audio_file(&audio, &diarize, &model, timeout)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("refine task panicked: {e}")))?;
    let output = match refined {
        Ok(output) => output,
        // No remote speech to refine (silent / Me-only meeting): leave the existing live segments
        // in place and report the current speakers rather than 500-ing.
        Err(hearsay_inference::InferenceError::NoSpeech) => {
            let speakers = queries::list_speaker_rows(&state.pool, id).await?;
            return Ok(Json(speaker_page(
                speakers.into_iter().map(SpeakerRead::from).collect(),
            )));
        }
        Err(e) => return Err(ApiError::Internal(format!("refine failed: {e}"))),
    };

    let result = queries::RefineResult {
        segments: output
            .segments
            .into_iter()
            .map(|s| queries::RefinedThemSegment {
                ordinal: s.ordinal,
                text: s.text,
                start_s: s.start_s,
                end_s: s.end_s,
            })
            .collect(),
        centroids: output.centroids,
    };
    // Apply the same effective recognition threshold the auto-refine uses (stored override else
    // config default), so manual and automatic re-diarization recognize returning speakers alike.
    let (_auto_refine, threshold) = queries::effective_speakers(
        &state.pool,
        state.settings.auto_refine,
        state.settings.recognition_threshold,
    )
    .await?;
    queries::replace_them_segments(&state.pool, id, &result, threshold).await?;

    // Regenerate transcript.md + meeting.json from the refined (+ Me) segments (off the async worker).
    let segments = queries::list_segments(&state.pool, id).await?;
    let write = tokio::task::spawn_blocking(move || {
        hearsay_orchestrator::write_meeting_files(&dir, &meeting, &segments)
    })
    .await;
    match write {
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "failed to rewrite transcript after rediarize")
        }
        Err(err) => tracing::warn!(error = %err, "transcript rewrite task panicked"),
        Ok(Ok(())) => {}
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
    Ok(Json(Page {
        total,
        page: window.page,
        page_size: window.page_size,
        items: rows.into_iter().map(IdentityRead::from).collect(),
    }))
}
