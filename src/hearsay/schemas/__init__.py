"""Pydantic request/response schemas (the API boundary)."""

from __future__ import annotations

from hearsay.schemas.common import Page
from hearsay.schemas.meeting import MeetingCreate, MeetingRead, MeetingRelocate
from hearsay.schemas.segment import SegmentRead, TranscriptEvent
from hearsay.schemas.settings import (
    AboutInfo,
    PermissionsInfo,
    RecordingSettings,
    SettingsRead,
    SpeakerSettings,
    StorageInfo,
    StorageSettings,
)
from hearsay.schemas.speaker import IdentityRead, SpeakerRead, SpeakerRename

__all__ = [
    "AboutInfo",
    "IdentityRead",
    "MeetingCreate",
    "MeetingRead",
    "MeetingRelocate",
    "Page",
    "PermissionsInfo",
    "RecordingSettings",
    "SegmentRead",
    "SettingsRead",
    "SpeakerRead",
    "SpeakerRename",
    "SpeakerSettings",
    "StorageInfo",
    "StorageSettings",
    "TranscriptEvent",
]
