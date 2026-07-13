"""LiveThemProcessor: the sidecar's turn segments are persisted, labeled, and clustered.

The subprocess I/O (spawn/feed/read/close) is validated on-device; here we drive the
``_handle`` core directly to cover speaker->label mapping, the meeting-time offset, cluster
reuse per speaker, and persistence.
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from datetime import UTC, datetime
from pathlib import Path
from uuid import uuid4

import pytest_asyncio
from sqlalchemy import create_engine as create_sync_engine

from hearsay.db import Database
from hearsay.enums import Stream
from hearsay.export import MeetingMeta, TranscriptLine
from hearsay.models import Base
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript.broadcast import Broadcaster
from hearsay.transcript.live import LiveThemProcessor


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
    db_file = tmp_path / "live.db"
    sync_engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(sync_engine)
    sync_engine.dispose()
    db = Database(f"sqlite+aiosqlite:///{db_file}")
    yield db
    await db.dispose()


async def test_handle_persists_labels_and_reuses_clusters(database: Database) -> None:
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="M", folder="m", storage_root="/out", started_at=datetime.now(UTC)
        )
    sink = _FakeSink()
    processor = LiveThemProcessor(
        binary_path=Path("unused"),
        meeting_id=meeting.id,
        database=database,
        broadcaster=Broadcaster(),
        sink=sink,
    )
    processor._offset_s = 10.0  # first Them sample is at meeting time 10s

    await processor._handle({"speaker": 0, "text": "hello", "start_s": 1.0, "end_s": 2.0})
    await processor._handle({"speaker": 1, "text": "hi", "start_s": 2.0, "end_s": 3.0})
    await processor._handle({"speaker": 0, "text": "more", "start_s": 3.0, "end_s": 4.0})

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=100)
        clusters = await SpeakerService(session).list_clusters(meeting.id)

    them = sorted(segments, key=lambda s: s.start_s)
    assert all(s.stream is Stream.THEM for s in them)
    # speaker 0 -> "Speaker 1", speaker 1 -> "Speaker 2"; times shifted by the +10s offset.
    assert [(s.speaker_label, s.text, s.start_s) for s in them] == [
        ("Speaker 1", "hello", 11.0),
        ("Speaker 2", "hi", 12.0),
        ("Speaker 1", "more", 13.0),
    ]
    # speaker 0's two turns reuse one cluster -> exactly two clusters total.
    assert sorted(c.ordinal for c in clusters) == [1, 2]
    assert them[0].cluster_id == them[2].cluster_id  # both "Speaker 1"
    assert len(sink.lines) == 3  # each turn appended to the transcript


async def test_partials_broadcast_but_not_persisted(database: Database) -> None:
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="M", folder="m", storage_root="/out", started_at=datetime.now(UTC)
        )
    sink = _FakeSink()
    broadcaster = Broadcaster()
    processor = LiveThemProcessor(
        binary_path=Path("unused"),
        meeting_id=meeting.id,
        database=database,
        broadcaster=broadcaster,
        sink=sink,
    )

    with broadcaster.subscribe() as queue:
        # A speaker-less partial (speaker -1) streams to the UI only; the finalized turn is
        # persisted + labeled with the diarizer speaker.
        await processor._handle(
            {"kind": "partial", "speaker": -1, "text": "hello wor", "start_s": 1.0, "end_s": 1.5}
        )
        await processor._handle(
            {"kind": "final", "speaker": 0, "text": "hello world", "start_s": 1.0, "end_s": 2.0}
        )
        events = [json.loads(queue.get_nowait()) for _ in range(queue.qsize())]

    assert [(e["kind"], e["speaker_label"]) for e in events] == [
        ("partial", "Them"),
        ("final", "Speaker 1"),
    ]
    async with database.session() as session:
        segments, total = await MeetingService(session).list_segments(
            meeting.id, page=1, page_size=100
        )
        clusters = await SpeakerService(session).list_clusters(meeting.id)
    # Only the final is persisted (one segment, one cluster); the partial left no rows.
    assert total == 1 and segments[0].text == "hello world"
    assert [c.ordinal for c in clusters] == [1]
    assert len(sink.lines) == 1  # only the final was appended to the transcript


async def test_feed_survives_a_dead_sidecar_pipe(database: Database) -> None:
    """A sidecar that dies mid-meeting must not crash the pipeline: feed swallows the broken
    pipe, marks itself broken, and stops writing -- so the stop path can still finalize."""

    class _DeadStdin:
        def __init__(self) -> None:
            self.writes = 0

        def write(self, data: bytes) -> None:
            self.writes += 1

        async def drain(self) -> None:
            raise BrokenPipeError(32, "Broken pipe")

    class _FakeProc:
        def __init__(self) -> None:
            self.stdin = _DeadStdin()

    processor = LiveThemProcessor(
        binary_path=Path("unused"),
        meeting_id=uuid4(),  # feed never touches the DB
        database=database,
        broadcaster=Broadcaster(),
        sink=_FakeSink(),
    )
    proc = _FakeProc()
    processor._proc = proc  # type: ignore[assignment]

    await processor.feed([0.0, 0.0, 0.0], t0_s=0.0)  # broken pipe must not propagate
    assert processor._broken is True
    assert proc.stdin.writes == 1
    await processor.feed([0.0, 0.0, 0.0], t0_s=1.0)  # already broken -> no-op, no further write
    assert proc.stdin.writes == 1
