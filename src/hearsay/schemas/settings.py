"""Request/response schemas for the settings API (the editable-preferences boundary)."""

from __future__ import annotations

from pydantic import BaseModel


class RecordingSettings(BaseModel):
    """Recording & privacy. ``record`` is the single audio-retention switch: keep one
    timeline-accurate WAV per meeting (needed for playback + the post-meeting refine)."""

    record: bool = True


class SettingsRead(BaseModel):
    """The full editable settings, one field per panel/section (extended as panels land)."""

    recording: RecordingSettings
