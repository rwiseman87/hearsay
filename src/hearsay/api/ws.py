"""Live transcript WebSocket.

Auth mirrors REST but over the handshake: the Origin must be loopback and the
per-session token is passed as a ``?token=`` query parameter (browsers cannot set
Authorization headers on a WebSocket). Subscribers receive partial + final
transcript events for the active meeting as JSON text frames.
"""

from __future__ import annotations

from uuid import UUID

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from hearsay.api.context import AppContext
from hearsay.api.security import origin_allowed, token_matches
from hearsay.log import get_logger

_log = get_logger("hearsay.ws")
router = APIRouter()


@router.websocket("/ws/meetings/{meeting_id}")
async def meeting_ws(websocket: WebSocket, meeting_id: UUID) -> None:
    context: AppContext = websocket.app.state.ctx
    if not origin_allowed(websocket.headers.get("origin")):
        await websocket.close(code=1008)
        return
    if not token_matches(websocket.query_params.get("token"), context.session_token):
        await websocket.close(code=1008)
        return

    active = context.session_manager.active
    if active is None or active.meeting_id != meeting_id:
        # Accept then close so the client sees a clean 1000 rather than a handshake
        # rejection it cannot distinguish from an auth failure.
        await websocket.accept()
        await websocket.close(code=1000)
        return

    await websocket.accept()
    with active.broadcaster.subscribe() as queue:
        try:
            while True:
                await websocket.send_text(await queue.get())
        except WebSocketDisconnect:
            pass
