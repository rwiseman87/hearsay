from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pytest
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.models import Meeting
from hearsay.services import MeetingRelocationError, MeetingService


async def _meeting(session: AsyncSession, root: Path, *, folder: str = "m") -> Meeting:
    return await MeetingService(session).create(
        title="T", folder=folder, storage_root=str(root), started_at=datetime.now(UTC)
    )


def _populate(directory: Path, *, audio: bool = True) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "transcript.md").write_text("# T\n", encoding="utf-8")
    (directory / "meeting.json").write_text("{}", encoding="utf-8")
    if audio:
        (directory / "audio.wav").write_bytes(b"\x00" * 64)


async def test_relocate_updates_root_and_resyncs(session: AsyncSession, tmp_path: Path) -> None:
    old, new = tmp_path / "old", tmp_path / "new"
    meeting = await _meeting(session, old)
    _populate(old / "m")
    _populate(new / "m")
    svc = MeetingService(session)
    await svc.sync_manifest(meeting)  # manifest built at the old location

    result = await svc.relocate(meeting, str(new))

    assert result.storage_root == str(new.resolve())
    assert {a.rel_path for a in await svc.sync_manifest(result)} == {
        "transcript.md",
        "meeting.json",
        "audio.wav",
    }


async def test_relocate_missing_artifact_raises(session: AsyncSession, tmp_path: Path) -> None:
    old, new = tmp_path / "old", tmp_path / "new"
    meeting = await _meeting(session, old)
    _populate(old / "m", audio=True)
    _populate(new / "m", audio=False)  # the recorded audio was not moved
    svc = MeetingService(session)
    await svc.sync_manifest(meeting)

    with pytest.raises(MeetingRelocationError) as exc:
        await svc.relocate(meeting, str(new))
    assert exc.value.missing == ["audio.wav"]

    refreshed = await svc.get(meeting.id)
    assert refreshed is not None and refreshed.storage_root == str(old)  # unchanged on failure


async def test_relocate_rejects_relative_path(session: AsyncSession, tmp_path: Path) -> None:
    meeting = await _meeting(session, tmp_path / "old")
    with pytest.raises(MeetingRelocationError):
        await MeetingService(session).relocate(meeting, "relative/dir")


async def test_relocate_without_manifest_falls_back(session: AsyncSession, tmp_path: Path) -> None:
    # A meeting from before manifests existed (no asset rows) whose files already moved: the
    # current dir is gone, so validation falls back to requiring the transcript at the target.
    old, new = tmp_path / "old", tmp_path / "new"
    meeting = await _meeting(session, old)  # old/m is never created on disk
    (new / "m").mkdir(parents=True)
    (new / "m" / "transcript.md").write_text("# T\n", encoding="utf-8")

    result = await MeetingService(session).relocate(meeting, str(new))
    assert result.storage_root == str(new.resolve())
