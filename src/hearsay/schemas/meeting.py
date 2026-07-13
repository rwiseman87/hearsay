"""Request/response schemas for meetings."""

from __future__ import annotations

from datetime import datetime
from uuid import UUID

from pydantic import BaseModel, ConfigDict, Field

from hearsay.enums import MeetingStatus


class MeetingCreate(BaseModel):
    """Start a meeting. ``title`` defaults to a timestamp-derived name when omitted."""

    title: str | None = Field(default=None, max_length=255)


class MeetingRelocate(BaseModel):
    """Re-point a meeting's storage to ``new_root`` (an absolute directory the artifacts moved
    to). The app validates the artifacts are there and updates the stored root; it does not move
    files."""

    new_root: str = Field(min_length=1, max_length=1024)


class MeetingRead(BaseModel):
    model_config = ConfigDict(from_attributes=True)

    id: UUID
    title: str
    folder: str
    storage_root: str
    status: MeetingStatus
    started_at: datetime
    ended_at: datetime | None
    created_at: datetime
    updated_at: datetime
