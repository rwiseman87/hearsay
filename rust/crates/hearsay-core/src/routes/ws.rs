//! Live transcript WebSocket. Port of `src/hearsay/api/ws.py`.
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
    loop {
        match receiver.recv().await {
            Ok(text) => {
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break; // client disconnected
                }
            }
            Err(RecvError::Lagged(_)) => continue, // dropped some events; keep streaming
            Err(RecvError::Closed) => break,       // meeting ended
        }
    }
}
