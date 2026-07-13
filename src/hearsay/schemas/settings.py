"""Request/response schemas for the settings API (the editable-preferences boundary)."""

from __future__ import annotations

from pydantic import BaseModel, Field


class RecordingSettings(BaseModel):
    """Recording & privacy. ``record`` is the single audio-retention switch: keep one
    timeline-accurate WAV per meeting (needed for playback + the post-meeting refine)."""

    record: bool = True


class SpeakerSettings(BaseModel):
    """Speaker diarization. ``auto_refine`` re-diarizes each meeting at finalize;
    ``recognition_threshold`` is the cosine at/above which a refined speaker is auto-matched
    to a person named in a previous meeting (higher = stricter)."""

    auto_refine: bool = True
    recognition_threshold: float = Field(default=0.6, ge=0.0, le=1.0)


class SettingsRead(BaseModel):
    """The full editable settings, one field per panel/section (extended as panels land)."""

    recording: RecordingSettings
    speakers: SpeakerSettings
