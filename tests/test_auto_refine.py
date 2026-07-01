"""Auto-refine at finalize: stopping a meeting re-diarizes it automatically.

Now that the default offline diarizer is FluidAudio on the ANE (~seconds), the refine
runs inline when a meeting finalizes, so it ends with accurate labels without the manual
button. The real diarization is exercised on-device; here we mock ``rediarize_meeting`` and
assert the hook fires only when configured + a recording exists, and never breaks the stop.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from datetime import UTC, datetime
from pathlib import Path
from uuid import UUID

import pytest
import pytest_asyncio
from sqlalchemy import create_engine as create_sync_engine

from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.models import Base, Meeting
from hearsay.services import MeetingService
from hearsay.transcript import session as session_mod
from hearsay.transcript.refine import RefineResult
from hearsay.transcript.session import SessionManager


class _FakeCapture:
    """Capture stand-in; never used here (we only stop), but satisfies the factory type."""

    async def start(self) -> None:
        return None

    async def stop(self) -> None:
        return None

    @property
    def media(self) -> None:
        return None


@pytest_asyncio.fixture
async def database(tmp_path: Path) -> AsyncIterator[Database]:
    db_file = tmp_path / "auto.db"
    sync_engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(sync_engine)
    sync_engine.dispose()
    db = Database(f"sqlite+aiosqlite:///{db_file}")
    yield db
    await db.dispose()


async def _finalized_candidate(
    database: Database, settings: Settings, *, folder: str = "meet-1", them: bool = True
) -> Meeting:
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="M", folder=folder, started_at=datetime.now(UTC)
        )
    meeting_dir = settings.output_dir / folder
    meeting_dir.mkdir(parents=True, exist_ok=True)
    if them:
        (meeting_dir / "them.wav").write_bytes(b"")  # existence is all the pre-check needs
    return meeting


def _record_calls(calls: list[UUID]) -> object:
    async def fake_rediarize(
        meeting_id: UUID, *, database: Database, settings: Settings
    ) -> RefineResult:
        calls.append(meeting_id)
        return RefineResult(meeting_id=meeting_id, speaker_count=2, segments_relabeled=4)

    return fake_rediarize


async def test_auto_refine_runs_at_finalize(
    database: Database, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = Settings(output_dir=tmp_path / "out")
    meeting = await _finalized_candidate(database, settings)
    calls: list[UUID] = []
    monkeypatch.setattr(session_mod, "rediarize_meeting", _record_calls(calls))

    manager = SessionManager(database=database, settings=settings, capture_factory=_FakeCapture)
    result = await manager.stop_meeting(meeting.id)

    assert result is not None  # finalize succeeded
    assert calls == [meeting.id]  # and auto-refine fired


async def test_auto_refine_disabled_does_not_run(
    database: Database, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = Settings(output_dir=tmp_path / "out")
    settings.diarization.auto_refine = False
    meeting = await _finalized_candidate(database, settings)
    calls: list[UUID] = []
    monkeypatch.setattr(session_mod, "rediarize_meeting", _record_calls(calls))

    manager = SessionManager(database=database, settings=settings, capture_factory=_FakeCapture)
    await manager.stop_meeting(meeting.id)

    assert calls == []


async def test_auto_refine_skips_without_recording(
    database: Database, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = Settings(output_dir=tmp_path / "out")
    meeting = await _finalized_candidate(database, settings, them=False)
    calls: list[UUID] = []
    monkeypatch.setattr(session_mod, "rediarize_meeting", _record_calls(calls))

    manager = SessionManager(database=database, settings=settings, capture_factory=_FakeCapture)
    await manager.stop_meeting(meeting.id)

    assert calls == []  # no them.wav -> nothing to refine


async def test_auto_refine_error_does_not_break_stop(
    database: Database, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = Settings(output_dir=tmp_path / "out")
    meeting = await _finalized_candidate(database, settings)

    async def boom(meeting_id: UUID, *, database: Database, settings: Settings) -> RefineResult:
        raise RuntimeError("diarizer exploded")

    monkeypatch.setattr(session_mod, "rediarize_meeting", boom)

    manager = SessionManager(database=database, settings=settings, capture_factory=_FakeCapture)
    result = await manager.stop_meeting(meeting.id)

    assert result is not None  # the stop still succeeded despite the refine blowing up
