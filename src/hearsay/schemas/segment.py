"""Response schema for transcript segments + the live WebSocket event."""

from __future__ import annotations

from typing import Literal
from uuid import UUID

from pydantic import BaseModel, ConfigDict

from hearsay.enums import Stream


class SegmentRead(BaseModel):
    model_config = ConfigDict(from_attributes=True)

    id: UUID
    stream: Stream
    speaker_label: str
    text: str
    start_s: float
    end_s: float


class TranscriptEvent(BaseModel):
    """A partial or final segment pushed to the UI over the WebSocket.

    Partials stream to the UI only; finals are also persisted and appended to
    ``transcript.md`` (see the markdown sink). ``start_s`` / ``end_s`` are
    meeting-relative seconds on the helper's single monotonic clock.
    """

    kind: Literal["partial", "final"]
    stream: Stream
    speaker_label: str
    text: str
    start_s: float
    end_s: float
