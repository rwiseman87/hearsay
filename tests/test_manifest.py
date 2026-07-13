from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.enums import AssetKind
from hearsay.models import Meeting
from hearsay.services import MeetingService


async def _meeting(session: AsyncSession, root: Path, *, folder: str = "m") -> Meeting:
    return await MeetingService(session).create(
        title="T", folder=folder, storage_root=str(root), started_at=datetime.now(UTC)
    )


def _write(directory: Path, name: str, data: bytes) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / name).write_bytes(data)


async def test_sync_records_present_artifacts(session: AsyncSession, tmp_path: Path) -> None:
    meeting = await _meeting(session, tmp_path)
    folder = tmp_path / "m"
    _write(folder, "transcript.md", b"# T\n")
    _write(folder, "meeting.json", b"{}")
    _write(folder, "audio.wav", b"\x00" * 100)

    assets = await MeetingService(session).sync_manifest(meeting)

    by_path = {a.rel_path: a for a in assets}
    assert set(by_path) == {"transcript.md", "meeting.json", "audio.wav"}
    assert by_path["transcript.md"].kind is AssetKind.TRANSCRIPT
    assert by_path["meeting.json"].kind is AssetKind.METADATA
    assert by_path["audio.wav"].kind is AssetKind.AUDIO
    assert by_path["audio.wav"].size_bytes == 100


async def test_sync_omits_audio_when_not_recorded(session: AsyncSession, tmp_path: Path) -> None:
    meeting = await _meeting(session, tmp_path)
    folder = tmp_path / "m"
    _write(folder, "transcript.md", b"# T\n")
    _write(folder, "meeting.json", b"{}")

    assets = await MeetingService(session).sync_manifest(meeting)

    assert {a.rel_path for a in assets} == {"transcript.md", "meeting.json"}


async def test_sync_updates_size_and_drops_removed(session: AsyncSession, tmp_path: Path) -> None:
    meeting = await _meeting(session, tmp_path)
    folder = tmp_path / "m"
    _write(folder, "transcript.md", b"# T\n")
    _write(folder, "audio.wav", b"\x00" * 100)
    svc = MeetingService(session)
    await svc.sync_manifest(meeting)

    # Transcript grows (refine rewrote it) and the audio is removed.
    _write(folder, "transcript.md", b"# T\n\nmore content\n")
    (folder / "audio.wav").unlink()

    assets = await svc.sync_manifest(meeting)

    by_path = {a.rel_path: a for a in assets}
    assert set(by_path) == {"transcript.md"}  # audio row dropped
    assert by_path["transcript.md"].size_bytes == len(b"# T\n\nmore content\n")


async def test_sync_empty_folder_is_noop(session: AsyncSession, tmp_path: Path) -> None:
    meeting = await _meeting(session, tmp_path)  # folder never created on disk
    assets = await MeetingService(session).sync_manifest(meeting)
    assert assets == []
