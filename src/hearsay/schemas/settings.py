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


class StorageSettings(BaseModel):
    """Storage location. ``output_dir`` is the default root new meetings are written under
    (existing meetings keep their stamped location); relocate an individual meeting from its
    own view."""

    output_dir: str = Field(min_length=1)


class StorageInfo(BaseModel):
    """Read-only storage facts shown alongside the editable section."""

    output_dir: str
    database_path: str
    tracked_bytes: int
    meeting_count: int


class AboutInfo(BaseModel):
    """Read-only build/runtime facts for the About panel (no persistence).

    ``protocol_version`` is the core's IPC frame-protocol constant; the helper reports
    its own copy on connect (surfaced by the Permissions panel), and a mismatch signals
    a helper/core version drift.
    """

    app_version: str
    environment: str
    protocol_version: int
    database_path: str


class PermissionsInfo(BaseModel):
    """Live TCC permission status probed from the capture helper (not a stored preference).

    Each permission is ``granted`` / ``denied`` / ``undetermined`` (from the helper's
    ``check_permissions``), or ``unknown`` when the helper is unavailable. Only the
    microphone has a real status today; ``audio_capture`` / ``screen_recording`` /
    ``accessibility`` / ``calendar`` are ``undetermined`` stubs until their capture phases
    land. ``helper_version`` is the connected helper's build; ``None`` when unavailable.
    """

    helper_available: bool
    helper_version: str | None = None
    microphone: str
    audio_capture: str
    screen_recording: str
    accessibility: str
    calendar: str


class SettingsRead(BaseModel):
    """The full editable settings, one field per panel/section (extended as panels land).

    ``storage_info`` and ``about`` are read-only context (not editable sections).
    """

    recording: RecordingSettings
    speakers: SpeakerSettings
    storage: StorageSettings
    storage_info: StorageInfo
    about: AboutInfo
