from __future__ import annotations

import asyncio
import json
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path
from types import SimpleNamespace

from sqlalchemy import create_engine as create_sync_engine

from hearsay.asr import ASRSegment
from hearsay.config.settings import VADSettings
from hearsay.db import Database
from hearsay.enums import Stream
from hearsay.export import LocalMarkdownSink, MeetingMeta
from hearsay.helper.media_channel import AudioChunk
from hearsay.models import Base
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript import Broadcaster, MeetingDiarizer, TranscriptionPipeline
from hearsay.transcript.pipeline import _clean_text

FRAME = 160


class StubVAD:
    @property
    def frame_samples(self) -> int:
        return FRAME

    def reset(self) -> None:
        return None

    def speech_prob(self, frame: Sequence[float]) -> float:
        return 1.0 if any(sample != 0.0 for sample in frame) else 0.0


class FakeASR:
    name = "fake"
    model = "fake"

    def transcribe(
        self, samples: Sequence[float], *, language: str | None = None
    ) -> list[ASRSegment]:
        return [ASRSegment(text="hello world", start_s=0.0, end_s=1.0)]


class StubEmbedder:
    """Maps an utterance's sample level to a distinct one-hot vector, so different
    'speakers' (positive vs negative level) cluster apart without a real model."""

    dim = 4
    model_id = "stub"

    def embed(self, samples: Sequence[float]) -> list[float]:
        level = next((s for s in samples if s != 0.0), 0.0)
        return [1.0, 0.0, 0.0, 0.0] if level >= 0.0 else [0.0, 1.0, 0.0, 0.0]


def _speech(frames: int) -> list[float]:
    return [0.5] * (frames * FRAME)


def _level(frames: int, value: float) -> list[float]:
    return [value] * (frames * FRAME)


def _silence(frames: int) -> list[float]:
    return [0.0] * (frames * FRAME)


def test_clean_text_drops_pure_non_speech() -> None:
    assert _clean_text("[BLANK_AUDIO]") == ""
    assert _clean_text("  [Music] ") == ""
    assert _clean_text("(buzzing)") == ""
    assert _clean_text("  hello world  ") == "hello world"
    assert _clean_text("It was $93,000.") == "It was $93,000."


def _make_db(tmp_path: Path) -> Database:
    db_file = tmp_path / "pipeline.db"
    engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(engine)
    engine.dispose()
    return Database(f"sqlite+aiosqlite:///{db_file}")


async def test_pipeline_transcribes_persists_and_broadcasts(tmp_path: Path) -> None:
    database = _make_db(tmp_path)
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="T", folder="mtg", started_at=datetime.now(UTC)
        )

    me_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    them_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    utterance = tuple(_silence(2) + _speech(5) + _silence(5))
    me_queue.put_nowait(AudioChunk(host_ts=0, samples=utterance))
    me_queue.put_nowait(None)
    them_queue.put_nowait(None)
    media = SimpleNamespace(queues={Stream.ME: me_queue, Stream.THEM: them_queue})

    broadcaster = Broadcaster()
    pipeline = TranscriptionPipeline(
        meeting_id=meeting.id,
        database=database,
        sink=LocalMarkdownSink(),
        broadcaster=broadcaster,
        asr=FakeASR(),
        vad_factory=StubVAD,
        vad=VADSettings(min_speech_ms=20, min_silence_ms=40, partial_ms=0),
    )
    meta = MeetingMeta(
        id=meeting.id, title="T", started_at=datetime.now(UTC), folder=tmp_path / "mtg"
    )

    events: list[dict[str, object]] = []
    with broadcaster.subscribe() as queue:
        await pipeline.open(media, meta)  # type: ignore[arg-type]
        await asyncio.sleep(0.2)  # let the per-stream consumers drain to eos
        await pipeline.close(ended_at=datetime.now(UTC))
        while not queue.empty():
            events.append(json.loads(queue.get_nowait()))

    async with database.session() as session:
        segments, total = await MeetingService(session).list_segments(
            meeting.id, page=1, page_size=10
        )
    await database.dispose()

    assert total == 1
    assert segments[0].text == "hello world"
    assert segments[0].speaker_label == "Me"
    assert segments[0].stream is Stream.ME

    transcript = (tmp_path / "mtg" / "transcript.md").read_text(encoding="utf-8")
    assert "hello world" in transcript
    assert "— Me" in transcript

    finals = [e for e in events if e["kind"] == "final"]
    assert len(finals) == 1
    assert finals[0]["text"] == "hello world"
    assert finals[0]["stream"] == "me"


async def test_pipeline_diarizes_them_and_labels_me_by_channel(tmp_path: Path) -> None:
    database = _make_db(tmp_path)
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="T", folder="mtg", started_at=datetime.now(UTC)
        )

    me_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    them_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    me_queue.put_nowait(
        AudioChunk(host_ts=0, samples=tuple(_silence(2) + _level(5, 0.5) + _silence(5)))
    )
    me_queue.put_nowait(None)
    # Two distinct Them speakers (positive vs negative level), one second apart.
    them_queue.put_nowait(
        AudioChunk(host_ts=0, samples=tuple(_silence(2) + _level(5, 0.5) + _silence(5)))
    )
    them_queue.put_nowait(AudioChunk(host_ts=10**9, samples=tuple(_level(5, -0.5) + _silence(5))))
    them_queue.put_nowait(None)
    media = SimpleNamespace(queues={Stream.ME: me_queue, Stream.THEM: them_queue})

    diarizer = MeetingDiarizer(
        meeting_id=meeting.id,
        database=database,
        embedder=StubEmbedder(),
        threshold=0.5,
        min_embed_ms=0,
    )
    pipeline = TranscriptionPipeline(
        meeting_id=meeting.id,
        database=database,
        sink=LocalMarkdownSink(),
        broadcaster=Broadcaster(),
        asr=FakeASR(),
        vad_factory=StubVAD,
        vad=VADSettings(min_speech_ms=20, min_silence_ms=40, partial_ms=0),
        diarizer=diarizer,
    )
    meta = MeetingMeta(
        id=meeting.id, title="T", started_at=datetime.now(UTC), folder=tmp_path / "mtg"
    )

    await pipeline.open(media, meta)  # type: ignore[arg-type]
    await asyncio.sleep(0.3)
    await pipeline.close(ended_at=datetime.now(UTC))

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=10)
        clusters = await SpeakerService(session).list_clusters(meeting.id)
    await database.dispose()

    me = [s for s in segments if s.stream is Stream.ME]
    them = [s for s in segments if s.stream is Stream.THEM]
    assert len(me) == 1 and me[0].speaker_label == "Me" and me[0].cluster_id is None
    assert len(them) == 2
    assert {s.speaker_label for s in them} == {"Speaker 1", "Speaker 2"}
    assert all(s.cluster_id is not None for s in them)
    assert sorted(c.ordinal for c in clusters) == [1, 2]


async def test_pipeline_without_diarizer_keeps_them_generic(tmp_path: Path) -> None:
    database = _make_db(tmp_path)
    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="T", folder="mtg", started_at=datetime.now(UTC)
        )

    me_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    them_queue: asyncio.Queue[AudioChunk | None] = asyncio.Queue()
    me_queue.put_nowait(None)
    them_queue.put_nowait(
        AudioChunk(host_ts=0, samples=tuple(_silence(2) + _level(5, 0.5) + _silence(5)))
    )
    them_queue.put_nowait(None)
    media = SimpleNamespace(queues={Stream.ME: me_queue, Stream.THEM: them_queue})

    pipeline = TranscriptionPipeline(  # no diarizer -> graceful degradation to "Them"
        meeting_id=meeting.id,
        database=database,
        sink=LocalMarkdownSink(),
        broadcaster=Broadcaster(),
        asr=FakeASR(),
        vad_factory=StubVAD,
        vad=VADSettings(min_speech_ms=20, min_silence_ms=40, partial_ms=0),
    )
    meta = MeetingMeta(
        id=meeting.id, title="T", started_at=datetime.now(UTC), folder=tmp_path / "mtg"
    )

    await pipeline.open(media, meta)  # type: ignore[arg-type]
    await asyncio.sleep(0.3)
    await pipeline.close(ended_at=datetime.now(UTC))

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=10)
        clusters = await SpeakerService(session).list_clusters(meeting.id)
    await database.dispose()

    them = [s for s in segments if s.stream is Stream.THEM]
    assert len(them) == 1 and them[0].speaker_label == "Them" and them[0].cluster_id is None
    assert clusters == []
