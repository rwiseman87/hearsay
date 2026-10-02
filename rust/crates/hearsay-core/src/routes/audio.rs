//! Meeting audio playback: serve the mixed recording so the UI can replay a meeting.
//!
//! A fresh meeting's `audio.wav` is served as a file. An archived `audio.flac` is decoded on demand
//! and served as the same WAV, because WebKit seeks FLAC by estimating byte offsets, which lands
//! tens of seconds off on unevenly compressible audio; WAV byte offsets map to time exactly.
//!
//! Auth accepts the per-session token either as the `?token=` query param (an `<audio>` element
//! cannot set an Authorization header, so the browser passes it in the URL, mirroring the
//! WebSocket) or as a normal bearer header. Both forms honour single `Range` requests for seeking.

use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use hearsay_audio::{AudioError, FlacIndex, AUDIO_FLAC};
use hearsay_db::queries;

use crate::extract::Path;
use crate::security::{bearer_token, query_token, token_matches};
use crate::state::AppState;

/// Frame indexes of recently played archives, keyed by path and file identity; a few KB each.
const INDEX_CACHE_LEN: usize = 4;

type IndexKey = (PathBuf, u64, Option<SystemTime>);
type IndexCache = Vec<(IndexKey, Arc<FlacIndex>)>;

static INDEXES: LazyLock<Mutex<IndexCache>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Routes served under the `/api` prefix (auth is inline, not the bearer route layer).
pub fn router() -> Router<AppState> {
    Router::new().route("/meetings/{id}/audio", get(get_meeting_audio))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "detail": "invalid or missing token" })),
    )
        .into_response()
}

async fn get_meeting_audio(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Response {
    let token = query_token(request.uri().query()).or_else(|| {
        bearer_token(
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
        )
    });
    if !token_matches(token, state.session_token.as_str()) {
        return unauthorized();
    }

    let meeting = match queries::get_meeting(&state.pool, id).await {
        Ok(Some(meeting)) => meeting,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "detail": "meeting not found" })),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, "database error serving audio");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let dir = meeting.dir_path(&state.settings.output_dir);
    let Some(audio_path) = hearsay_audio::resolve_recorded_audio(&dir) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "no audio recorded for this meeting" })),
        )
            .into_response();
    };

    if audio_path
        .file_name()
        .is_some_and(|name| name == AUDIO_FLAC)
    {
        let range = request
            .headers()
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        return serve_flac_as_wav(audio_path, range.as_deref()).await;
    }

    match ServeFile::new(audio_path).oneshot(request).await {
        Ok(response) => response.into_response(),
        Err(infallible) => match infallible {},
    }
}

/// The index for `path`, built once per file version and kept for the last few played.
fn flac_index(path: &FsPath) -> Result<Arc<FlacIndex>, AudioError> {
    let meta = std::fs::metadata(path)?;
    let key: IndexKey = (path.to_path_buf(), meta.len(), meta.modified().ok());
    let mut cache = INDEXES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(pos) = cache.iter().position(|(k, _)| *k == key) {
        let entry = cache.remove(pos);
        let index = Arc::clone(&entry.1);
        cache.push(entry);
        return Ok(index);
    }
    drop(cache);
    let index = Arc::new(FlacIndex::build(path)?);
    let mut cache = INDEXES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|(k, _)| k.0 != key.0);
    cache.push((key, Arc::clone(&index)));
    if cache.len() > INDEX_CACHE_LEN {
        cache.remove(0);
    }
    Ok(index)
}

/// A single `bytes=` range resolved against `len`.
#[derive(Debug, PartialEq, Eq)]
enum ByteRange {
    Full,
    Partial(u64, u64),
    Unsatisfiable,
}

/// Parse a `Range` header. Anything malformed or multi-range is ignored (a full `200`), as RFC 9110
/// allows; a range starting past the end is `416`.
fn parse_range(header: Option<&str>, len: u64) -> ByteRange {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return ByteRange::Full;
    };
    if spec.contains(',') {
        return ByteRange::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return ByteRange::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        return match last.parse::<u64>() {
            Ok(0) => ByteRange::Unsatisfiable,
            Ok(n) if len > 0 => ByteRange::Partial(len - n.min(len), len - 1),
            Ok(_) => ByteRange::Unsatisfiable,
            Err(_) => ByteRange::Full,
        };
    }
    let Ok(start) = first.parse::<u64>() else {
        return ByteRange::Full;
    };
    let end = if last.is_empty() {
        len.saturating_sub(1)
    } else {
        match last.parse::<u64>() {
            Ok(end) if end >= start => end.min(len.saturating_sub(1)),
            _ => return ByteRange::Full,
        }
    };
    if start >= len {
        ByteRange::Unsatisfiable
    } else {
        ByteRange::Partial(start, end)
    }
}

async fn serve_flac_as_wav(path: PathBuf, range_header: Option<&str>) -> Response {
    let index = {
        let path = path.clone();
        match tokio::task::spawn_blocking(move || flac_index(&path)).await {
            Ok(Ok(index)) => index,
            Ok(Err(err)) => {
                tracing::error!(error = %err, "could not index archived audio");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            Err(err) => {
                tracing::error!(error = %err, "audio index task failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    };
    let len = index.wav_len();
    let (status, start, end) = match parse_range(range_header, len) {
        ByteRange::Full => (StatusCode::OK, 0, len.saturating_sub(1)),
        ByteRange::Partial(start, end) => (StatusCode::PARTIAL_CONTENT, start, end),
        ByteRange::Unsatisfiable => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Body::empty())
                .expect("static response parts are valid");
        }
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::task::spawn_blocking(move || {
        let result = index.read_wav_range(&path, start..=end, |chunk| {
            tx.blocking_send(Ok(Bytes::copy_from_slice(chunk)))
                .map_err(|_| AudioError::Io(std::io::ErrorKind::BrokenPipe.into()))
        });
        if let Err(err) = result {
            // A closed receiver means the player moved on; anything else ends the body early.
            if !matches!(&err, AudioError::Io(e) if e.kind() == std::io::ErrorKind::BrokenPipe) {
                tracing::error!(error = %err, "decoding archived audio failed mid-stream");
                let _ = tx.blocking_send(Err(std::io::Error::other(err.to_string())));
            }
        }
    });

    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"))
        .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
        .header(header::CONTENT_LENGTH, end - start + 1);
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    response
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .expect("response parts are valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_range_forms_a_player_sends() {
        assert_eq!(parse_range(None, 100), ByteRange::Full);
        assert_eq!(
            parse_range(Some("bytes=0-1"), 100),
            ByteRange::Partial(0, 1)
        );
        assert_eq!(
            parse_range(Some("bytes=10-"), 100),
            ByteRange::Partial(10, 99)
        );
        assert_eq!(
            parse_range(Some("bytes=90-500"), 100),
            ByteRange::Partial(90, 99)
        );
        assert_eq!(
            parse_range(Some("bytes=-30"), 100),
            ByteRange::Partial(70, 99)
        );
        assert_eq!(
            parse_range(Some("bytes=-300"), 100),
            ByteRange::Partial(0, 99)
        );
    }

    #[test]
    fn rejects_a_start_past_the_end_and_ignores_malformed_ranges() {
        assert_eq!(
            parse_range(Some("bytes=100-"), 100),
            ByteRange::Unsatisfiable
        );
        assert_eq!(parse_range(Some("bytes=-0"), 100), ByteRange::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=5-2"), 100), ByteRange::Full);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), ByteRange::Full);
        assert_eq!(parse_range(Some("items=0-1"), 100), ByteRange::Full);
        assert_eq!(parse_range(Some("bytes=x-1"), 100), ByteRange::Full);
    }
}
