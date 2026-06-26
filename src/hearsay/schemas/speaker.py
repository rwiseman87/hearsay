"""Speaker (diarization cluster) + identity schemas."""

from __future__ import annotations

from uuid import UUID

from pydantic import BaseModel, ConfigDict, Field


class SpeakerRead(BaseModel):
    """A diarization cluster within a meeting, with its resolved display label."""

    id: UUID  # the cluster id
    ordinal: int
    label: str  # the bound identity's name, else "Speaker N"
    identity_id: UUID | None
    locked: bool


class SpeakerRename(BaseModel):
    """Rename a cluster to a person (binds + locks; relabels that speaker's segments)."""

    model_config = ConfigDict(str_strip_whitespace=True)

    display_name: str = Field(min_length=1, max_length=255)


class IdentityRead(BaseModel):
    """A known cross-meeting person (offered as a rename suggestion)."""

    model_config = ConfigDict(from_attributes=True)

    id: UUID
    display_name: str
    email: str | None
