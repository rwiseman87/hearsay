//! Live transcript WebSocket.
//!
//! Auth mirrors REST but over the handshake: the Origin must be loopback and the per-session token
//! is passed as a `?token=` query parameter (browsers cannot set Authorization headers on a
//! WebSocket). Subscribers receive partial + final transcript events for the active meeting as JSON
//! text frames. When the requested meeting is not the active recording session, the socket is
//! accepted then closed cleanly (a plain 1000), so the client can tell that apart from an auth
//! failure.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::security::{origin_allowed, token_matches};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// The `/ws/...` route (auth is inline; the global loopback layer still applies).
pub fn router() -> Router<AppState> {
    Router::new().route("/ws/meetings/{id}", get(meeting_ws))
}

async fn meeting_ws(
    State(state): State<AppState>,
    Path(meeting_id): Path<Uuid>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !origin_allowed(origin) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    if !token_matches(query.token.as_deref(), state.session_token.as_str()) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    upgrade.on_upgrade(move |socket| stream_transcript(socket, state, meeting_id))
}

async fn stream_transcript(mut socket: WebSocket, state: AppState, meeting_id: Uuid) {
    let Some(mut receiver) = state.engine.subscribe(meeting_id) else {
        // Not the active session: accept then close with a normal 1000.
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    // Warm-up snapshot: if the transcription sidecars are still loading their models (a cold start),
    // tell this subscriber up front so it shows a "preparing" notice instead of a silent gap. Read
    // *after* subscribing, so the `ready` transition (broadcast to the receiver above) can never be
    // missed in the gap between the two. A pre-warmed meeting reports not-warming and sends nothing.
    if state.engine.transcription_warming(meeting_id) == Some(true) {
        let frame = r#"{"kind":"status","state":"warming"}"#;
        if socket.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
    // Inactivity-prompt snapshot: if the meeting is currently in a silence prompt (a user reopening
    // the window mid-silence), tell this subscriber up front so it shows the "still recording?" banner
    // immediately rather than waiting for the next broadcast. Read after subscribing, like the warm-up
    // snapshot, so a prompt broadcast can never be missed in the gap.
    if let Some(silent_seconds) = state.engine.inactivity_prompt(meeting_id) {
        let frame = format!(r#"{{"kind":"prompt","silent_seconds":{silent_seconds}}}"#);
        if socket.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
    // Pause snapshot: if the meeting is currently paused (a user reopening the window mid-pause), tell
    // this subscriber so it shows the paused state (frozen timer/waveform, "Resume") immediately.
    if state.engine.paused(meeting_id) == Some(true) {
        let frame = r#"{"kind":"capture_state","state":"paused"}"#;
        if socket.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
    loop {
        match receiver.recv().await {
            Ok(text) => {
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break; // client disconnected
                }
            }
            Err(RecvError::Lagged(_)) => {
                // A slow/backpressured subscriber fell behind and the broadcast buffer dropped
                // events — which include persisted finals. Silently continuing would leave the live
                // view permanently short those lines until stop re-seeds. Since the pipeline
                // persists a final before broadcasting it, the DB is a superset of the stream, so
                // tell the client the persisted transcript is now ahead of this stream; it backfills
                // from `GET /segments` rather than diverging. Then keep streaming.
                let frame = r#"{"kind":"resync"}"#;
                if socket.send(Message::Text(frame.into())).await.is_err() {
                    break; // client disconnected
                }
            }
            Err(RecvError::Closed) => break, // meeting ended
        }
    }
}
