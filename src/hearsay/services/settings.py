"""Editable-settings logic: resolve + persist the user-preferences overlay.

The typed :class:`~hearsay.config.settings.Settings` is startup/env-driven and read-only. This
service overlays a writable :class:`~hearsay.models.preference.Preference` row per section: the
effective value is the stored override when present, otherwise the ``Settings`` default. The UI
edits the overlay; feature code reads the resolved value.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from sqlalchemy import func, select
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay import __version__
from hearsay.config.settings import Settings
from hearsay.helper.protocol import VERSION as PROTOCOL_VERSION
from hearsay.models import Meeting, MeetingAsset, Preference
from hearsay.schemas import (
    AboutInfo,
    RecordingSettings,
    SettingsRead,
    SpeakerSettings,
    StorageInfo,
    StorageSettings,
)

_RECORDING = "recording"
_SPEAKERS = "speakers"
_STORAGE = "storage"


class SettingsValidationError(ValueError):
    """A settings value was rejected (e.g. an output directory that is missing or not writable)."""


def _validate_output_dir(path_str: str) -> Path:
    """Resolve ``path_str`` to an absolute, existing, writable directory or raise."""
    path = Path(path_str).expanduser()
    if not path.is_absolute():
        raise SettingsValidationError("output_dir must be an absolute path")
    resolved = path.resolve()
    if not resolved.is_dir():
        raise SettingsValidationError(f"{resolved} is not an existing directory")
    probe = resolved / ".hearsay-write-test"
    try:
        probe.touch()
        probe.unlink()
    except OSError as exc:
        raise SettingsValidationError(f"{resolved} is not writable") from exc
    return resolved


def _database_path(settings: Settings) -> str:
    """The local DB file path for display; avoid leaking credentials for a remote DB URL."""
    url = settings.database_url or ""
    if url.startswith("sqlite"):
        return url.split(":///", 1)[-1]
    return "(external database)"


class SettingsService:
    def __init__(self, session: AsyncSession) -> None:
        self._session = session

    async def read_all(self, settings: Settings) -> SettingsRead:
        """The effective settings across every section (stored override or default)."""
        return SettingsRead(
            recording=await self.recording(settings),
            speakers=await self.speakers(settings),
            storage=await self.storage(settings),
            storage_info=await self.storage_info(settings),
            about=self.about(settings),
        )

    def about(self, settings: Settings) -> AboutInfo:
        """Read-only build/runtime facts (app version, environment, IPC protocol, DB path)."""
        return AboutInfo(
            app_version=__version__,
            environment=settings.environment.value,
            protocol_version=PROTOCOL_VERSION,
            database_path=_database_path(settings),
        )

    async def recording(self, settings: Settings) -> RecordingSettings:
        stored = await self._section(_RECORDING)
        if stored is not None:
            return RecordingSettings.model_validate(stored)
        return RecordingSettings(record=settings.audio.record)

    async def set_recording(self, patch: RecordingSettings) -> RecordingSettings:
        await self._upsert(_RECORDING, patch.model_dump())
        return patch

    async def effective_audio_record(self, settings: Settings) -> bool:
        """The audio-retention switch feature code reads at meeting start."""
        return (await self.recording(settings)).record

    async def speakers(self, settings: Settings) -> SpeakerSettings:
        stored = await self._section(_SPEAKERS)
        if stored is not None:
            return SpeakerSettings.model_validate(stored)
        return SpeakerSettings(
            auto_refine=settings.diarization.auto_refine,
            recognition_threshold=settings.diarization.recognition_threshold,
        )

    async def set_speakers(self, patch: SpeakerSettings) -> SpeakerSettings:
        await self._upsert(_SPEAKERS, patch.model_dump())
        return patch

    async def effective_auto_refine(self, settings: Settings) -> bool:
        """Whether the offline refine runs automatically at finalize."""
        return (await self.speakers(settings)).auto_refine

    async def effective_recognition_threshold(self, settings: Settings) -> float:
        """The cosine threshold the refine uses to auto-match a returning speaker."""
        return (await self.speakers(settings)).recognition_threshold

    async def storage(self, settings: Settings) -> StorageSettings:
        stored = await self._section(_STORAGE)
        if stored is not None:
            return StorageSettings.model_validate(stored)
        return StorageSettings(output_dir=str(settings.output_dir))

    async def set_storage(self, patch: StorageSettings) -> StorageSettings:
        """Validate the target directory (absolute, existing, writable) and store it."""
        resolved = await asyncio.to_thread(_validate_output_dir, patch.output_dir)
        stored = StorageSettings(output_dir=str(resolved))
        await self._upsert(_STORAGE, stored.model_dump())
        return stored

    async def effective_output_dir(self, settings: Settings) -> Path:
        """The default root new meetings are stamped under."""
        return Path((await self.storage(settings)).output_dir)

    async def storage_info(self, settings: Settings) -> StorageInfo:
        """Read-only storage facts: default root, DB path, and totals from the manifest."""
        tracked = await self._session.scalar(select(func.sum(MeetingAsset.size_bytes))) or 0
        count = await self._session.scalar(select(func.count()).select_from(Meeting)) or 0
        return StorageInfo(
            output_dir=str((await self.storage(settings)).output_dir),
            database_path=_database_path(settings),
            tracked_bytes=int(tracked),
            meeting_count=int(count),
        )

    async def _section(self, section: str) -> dict[str, Any] | None:
        pref = await self._session.scalar(select(Preference).where(Preference.section == section))
        return dict(pref.value) if pref is not None else None

    async def _upsert(self, section: str, value: dict[str, Any]) -> None:
        pref = await self._session.scalar(select(Preference).where(Preference.section == section))
        if pref is None:
            self._session.add(Preference(section=section, value=value))
        else:
            pref.value = value
        await self._session.commit()
