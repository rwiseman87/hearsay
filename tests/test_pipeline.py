from __future__ import annotations

import asyncio
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path
from types import SimpleNamespace

from sqlalchemy import create_engine as create_sync_engine

from hearsay.db import Database
from hearsay.enums import Stream
from hearsay.export import LocalMarkdownSink, MeetingMeta
from hearsay.helper.media_channel import AudioChunk
from hearsay.models import Base
from hearsay.services import MeetingService
from hearsay.transcript import TranscriptionPipeline

FRAME = 160


def _speech(frames: int) -> list[float]:
    return [0.5] * (frames * FRAME)


def _make_db(tmp_path: Path) -> Database:
    db_file = tmp_path / "pipeline.db"
    engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(engine)
    engine.dispose()
    return Database(f"sqlite+aiosqlite:///{db_file}")


class FakeProcessor:
    """Records the audio fed to it; stands in for a live sidecar (no subprocess)."""

    def __init__(self) -> None:
        self.fed: list[tuple[int, float]] = []
        self.started = False
        self.closed = False

    async def start(self) -> None:
        self.started = True

    async def feed(self, samples: Sequence[float], t0_s: float) -> None:
        self.fed.append((len(samples), t0_s))

    async def close(self) -> None:
        self.closed = True


class FakeRecorder:
    """Records the Them chunks written to it; stands in for ThemAudioRecorder."""

    def __init__(self) -> None:
        self.writes: list[tuple[int, float]] = []
        self.closed = False

    def write(self, samples: Sequence[float], *, t0_s: float) -> None:
        self.writes.append((len(samples), t0_s))

    def close(self) -> None:
        self.closed = True


async def test_pipeline_routes_each_stream_to_its_sidecar(tmp_path: Path) -> None:
    database = _make_db(tmp_path)
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="T", folder="mtg", started_at=datetime.now(UTC)
        )

    me_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    them_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    me_queue.put_nowait(AudioChunk(host_ts=0, samples=tuple(_speech(5))))
    me_queue.put_nowait(None)
    them_queue.put_nowait(AudioChunk(host_ts=0, samples=tuple(_speech(3))))
    them_queue.put_nowait(None)
    media = SimpleNamespace(queues={Stream.ME: me_queue, Stream.THEM: them_queue})

    me_processor = FakeProcessor()
    them_processor = FakeProcessor()
    them_recorder = FakeRecorder()
    pipeline = TranscriptionPipeline(
        meeting_id=meeting.id,
        database=database,
        sink=LocalMarkdownSink(),
        them_recorder=them_recorder,  # type: ignore[arg-type]
        them_processor=them_processor,  # type: ignore[arg-type]
        me_processor=me_processor,  # type: ignore[arg-type]
    )
    meta = MeetingMeta(
        id=meeting.id, title="T", started_at=datetime.now(UTC), folder=tmp_path / "mtg"
    )

    await pipeline.open(media, meta)  # type: ignore[arg-type]
    await asyncio.sleep(0.2)
    await pipeline.close(ended_at=datetime.now(UTC))

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=10)
    await database.dispose()

    # Each stream's PCM went to its own sidecar; both were started and drained on close.
    assert me_processor.started and me_processor.closed
    assert them_processor.started and them_processor.closed
    assert me_processor.fed == [(5 * FRAME, 0.0)]
    assert them_processor.fed == [(3 * FRAME, 0.0)]
    # Only the Them track is recorded (for the post-meeting refine); Me is never recorded.
    assert them_recorder.writes == [(3 * FRAME, 0.0)]
    assert them_recorder.closed
    # The fakes persist nothing, so the pipeline itself writes no segments (real sidecars do).
    assert segments == []


async def test_pipeline_drains_stream_without_a_processor(tmp_path: Path) -> None:
    """A stream with no sidecar (e.g. a missing binary) is drained without crashing."""
    database = _make_db(tmp_path)
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="T", folder="mtg", started_at=datetime.now(UTC)
        )

    me_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    them_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    me_queue.put_nowait(AudioChunk(host_ts=0, samples=tuple(_speech(5))))
    me_queue.put_nowait(None)
    them_queue.put_nowait(None)
    media = SimpleNamespace(queues={Stream.ME: me_queue, Stream.THEM: them_queue})

    # No processors at all: the consumers just drain to eos and finalize an empty transcript.
    pipeline = TranscriptionPipeline(
        meeting_id=meeting.id,
        database=database,
        sink=LocalMarkdownSink(),
    )
    meta = MeetingMeta(
        id=meeting.id, title="T", started_at=datetime.now(UTC), folder=tmp_path / "mtg"
    )

    await pipeline.open(media, meta)  # type: ignore[arg-type]
    await asyncio.sleep(0.2)
    await pipeline.close(ended_at=datetime.now(UTC))

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=10)
    await database.dispose()

    assert segments == []
