"""Request/response schemas for meetings."""

from __future__ import annotations

from datetime import datetime
from uuid import UUID

from pydantic import BaseModel, ConfigDict, Field

from hearsay.enums import MeetingStatus


class MeetingCreate(BaseModel):
    """Start a meeting. ``title`` defaults to a timestamp-derived name when omitted."""

    title: str | None = Field(default=None, max_length=255)


class MeetingRead(BaseModel):
    model_config = ConfigDict(from_attributes=True)

    id: UUID
    title: str
    folder: str
    status: MeetingStatus
    started_at: datetime
    ended_at: datetime | None
    created_at: datetime
    updated_at: datetime
