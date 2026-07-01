"""Meeting audio playback: serve the mixed WAV so the UI can replay a meeting.

Auth accepts the per-session token either as the ``?token=`` query param (an ``<audio>``
element cannot set an Authorization header, so the browser passes it in the URL, mirroring the
WebSocket) or as a normal bearer header. Starlette's ``FileResponse`` honours Range requests,
so the browser can seek.
"""

from __future__ import annotations

from uuid import UUID

from fastapi import APIRouter, HTTPException, Request, status
from fastapi.responses import FileResponse

from hearsay.api.context import AppContext
from hearsay.api.security import bearer_token, token_matches
from hearsay.services import MeetingService

router = APIRouter(prefix="/meetings", tags=["audio"])


@router.get("/{meeting_id}/audio")
async def get_meeting_audio(meeting_id: UUID, request: Request) -> FileResponse:
    context: AppContext = request.app.state.ctx
    token = request.query_params.get("token") or bearer_token(request.headers.get("authorization"))
    if not token_matches(token, context.session_token):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED, detail="invalid or missing token"
        )
    async with context.database.session() as session:
        meeting = await MeetingService(session).get(meeting_id)
    if meeting is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="meeting not found")
    audio_path = context.settings.output_dir / meeting.folder / "audio.wav"
    if not audio_path.exists():
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND, detail="no audio recorded for this meeting"
        )
    return FileResponse(audio_path, media_type="audio/wav")
