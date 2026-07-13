from __future__ import annotations

from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.config.settings import Settings
from hearsay.schemas import RecordingSettings, SpeakerSettings
from hearsay.services import SettingsService


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
