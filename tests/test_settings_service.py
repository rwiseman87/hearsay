from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pytest
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay import __version__
from hearsay.config.settings import Settings
from hearsay.enums import AssetKind
from hearsay.helper.protocol import VERSION as PROTOCOL_VERSION
from hearsay.models import MeetingAsset
from hearsay.schemas import RecordingSettings, SpeakerSettings, StorageSettings
from hearsay.services import MeetingService, SettingsService, SettingsValidationError


async def test_recording_defaults_to_settings(session: AsyncSession) -> None:
    settings = Settings()
    settings.audio.record = False  # env default
    recording = await SettingsService(session).recording(settings)
    assert recording.record is False  # no override stored -> falls back to the Settings default


async def test_set_recording_overrides_default(session: AsyncSession) -> None:
    settings = Settings()
    settings.audio.record = True
    svc = SettingsService(session)

    await svc.set_recording(RecordingSettings(record=False))

    assert (await svc.recording(settings)).record is False  # stored override wins over the default
    assert await svc.effective_audio_record(settings) is False


async def test_set_recording_is_idempotent_upsert(session: AsyncSession) -> None:
    settings = Settings()
    svc = SettingsService(session)
    await svc.set_recording(RecordingSettings(record=False))
    await svc.set_recording(RecordingSettings(record=True))  # updates the one row, not a 2nd
    assert await svc.effective_audio_record(settings) is True


async def test_read_all_includes_recording(session: AsyncSession) -> None:
    settings = Settings()
    settings.audio.record = True
    result = await SettingsService(session).read_all(settings)
    assert result.recording.record is True


async def test_speakers_default_from_settings(session: AsyncSession) -> None:
    settings = Settings()
    settings.diarization.auto_refine = False
    settings.diarization.recognition_threshold = 0.42
    speakers = await SettingsService(session).speakers(settings)
    assert speakers.auto_refine is False
    assert speakers.recognition_threshold == 0.42


async def test_set_speakers_overrides_defaults(session: AsyncSession) -> None:
    settings = Settings()  # defaults: auto_refine True, threshold 0.6
    svc = SettingsService(session)

    await svc.set_speakers(SpeakerSettings(auto_refine=False, recognition_threshold=0.8))

    assert await svc.effective_auto_refine(settings) is False
    assert await svc.effective_recognition_threshold(settings) == 0.8
    assert (await svc.read_all(settings)).speakers.recognition_threshold == 0.8


async def test_storage_default_and_override(session: AsyncSession, tmp_path: Path) -> None:
    settings = Settings(output_dir=tmp_path / "out")
    svc = SettingsService(session)
    # Default reflects settings.output_dir.
    assert (await svc.storage(settings)).output_dir == str(tmp_path / "out")

    new_root = tmp_path / "elsewhere"
    new_root.mkdir()
    stored = await svc.set_storage(StorageSettings(output_dir=str(new_root)))

    assert stored.output_dir == str(new_root.resolve())
    assert await svc.effective_output_dir(settings) == new_root.resolve()


async def test_set_storage_rejects_bad_dirs(session: AsyncSession, tmp_path: Path) -> None:
    svc = SettingsService(session)
    with pytest.raises(SettingsValidationError):
        await svc.set_storage(StorageSettings(output_dir="relative/dir"))  # not absolute
    with pytest.raises(SettingsValidationError):
        await svc.set_storage(StorageSettings(output_dir=str(tmp_path / "missing")))  # not a dir


async def test_storage_info_totals_from_manifest(session: AsyncSession, tmp_path: Path) -> None:
    settings = Settings(output_dir=tmp_path / "out")
    meeting = await MeetingService(session).create(
        title="T", folder="m", storage_root=str(tmp_path), started_at=datetime.now(UTC)
    )
    session.add(
        MeetingAsset(
            meeting_id=meeting.id, kind=AssetKind.AUDIO, rel_path="audio.wav", size_bytes=1000
        )
    )
    await session.commit()

    info = await SettingsService(session).storage_info(settings)
    assert info.tracked_bytes == 1000
    assert info.meeting_count == 1
    assert info.database_path.endswith(".db")


async def test_about_reports_build_facts(session: AsyncSession, tmp_path: Path) -> None:
    settings = Settings(output_dir=tmp_path / "out")
    about = SettingsService(session).about(settings)
    assert about.app_version == __version__
    assert about.environment == settings.environment.value
    assert about.protocol_version == PROTOCOL_VERSION
    assert about.database_path.endswith(".db")


async def test_read_all_includes_about(session: AsyncSession) -> None:
    result = await SettingsService(session).read_all(Settings())
    assert result.about.app_version == __version__
