"""Editable-settings logic: resolve + persist the user-preferences overlay.

The typed :class:`~hearsay.config.settings.Settings` is startup/env-driven and read-only. This
service overlays a writable :class:`~hearsay.models.preference.Preference` row per section: the
effective value is the stored override when present, otherwise the ``Settings`` default. The UI
edits the overlay; feature code reads the resolved value.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.config.settings import Settings
from hearsay.models import Preference
from hearsay.schemas import RecordingSettings, SettingsRead

_RECORDING = "recording"


class SettingsService:
    def __init__(self, session: AsyncSession) -> None:
        self._session = session

    async def read_all(self, settings: Settings) -> SettingsRead:
        """The effective settings across every section (stored override or default)."""
        return SettingsRead(recording=await self.recording(settings))

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
