"""LiveMeProcessor: the hearsay-me sidecar's utterances are persisted + broadcast as "Me".

The subprocess I/O (spawn/feed/read/close) is validated on-device; here we drive the
``_handle`` core directly to cover the "Me" label, the meeting-time offset, and persistence.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from datetime import UTC, datetime
from pathlib import Path

import pytest_asyncio
from sqlalchemy import create_engine as create_sync_engine

from hearsay.db import Database
from hearsay.enums import Stream
from hearsay.export import MeetingMeta, TranscriptLine
from hearsay.models import Base
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript.broadcast import Broadcaster
from hearsay.transcript.live_me import LiveMeProcessor


class _FakeSink:
    def __init__(self) -> None:
        self.lines: list[TranscriptLine] = []

    async def open(self, meta: MeetingMeta) -> None:
        return None

    async def append(self, line: TranscriptLine) -> None:
        self.lines.append(line)

    async def finalize(
        self, lines: list[TranscriptLine], *, ended_at: datetime, status: str
    ) -> None:
        return None


@pytest_asyncio.fixture
async def database(tmp_path: Path) -> AsyncIterator[Database]:
    db_file = tmp_path / "me.db"
    sync_engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(sync_engine)
    sync_engine.dispose()
    db = Database(f"sqlite+aiosqlite:///{db_file}")
    yield db
    await db.dispose()


async def test_handle_persists_me_with_offset(database: Database) -> None:
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="M", folder="m", started_at=datetime.now(UTC)
        )
    sink = _FakeSink()
    processor = LiveMeProcessor(
        binary_path=Path("unused"),
        meeting_id=meeting.id,
        database=database,
        broadcaster=Broadcaster(),
        sink=sink,
    )
    processor._offset_s = 5.0  # first Me sample is at meeting time 5s

    await processor._handle({"text": "hello there", "start_s": 1.0, "end_s": 2.0})
    await processor._handle({"text": "general kenobi", "start_s": 3.0, "end_s": 4.0})

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=100)
        clusters = await SpeakerService(session).list_clusters(meeting.id)

    me = sorted(segments, key=lambda s: s.start_s)
    # Me is always the local speaker (no diarization): "Me" label, no cluster, times +5s offset.
    assert [(s.stream, s.speaker_label, s.text, s.start_s) for s in me] == [
        (Stream.ME, "Me", "hello there", 6.0),
        (Stream.ME, "Me", "general kenobi", 8.0),
    ]
    assert all(s.cluster_id is None for s in me)
    assert clusters == []
    assert len(sink.lines) == 2  # each utterance appended to the transcript
