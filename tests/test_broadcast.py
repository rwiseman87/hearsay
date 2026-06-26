from __future__ import annotations

import asyncio
import json
from datetime import UTC, datetime
from pathlib import Path
from uuid import uuid4

from hearsay.enums import Stream
from hearsay.export import MeetingMeta
from hearsay.schemas import TranscriptEvent
from hearsay.transcript import Broadcaster, MeetingSession


class _DummyCapture:
    async def start(self) -> None:
        return None

    async def stop(self) -> None:
        return None

    @property
    def media(self) -> None:
        return None


async def test_broadcaster_fans_out_to_all_subscribers() -> None:
    broadcaster = Broadcaster()
    with broadcaster.subscribe() as q1, broadcaster.subscribe() as q2:
        broadcaster.publish("hello")
        assert await asyncio.wait_for(q1.get(), 1) == "hello"
        assert await asyncio.wait_for(q2.get(), 1) == "hello"


def test_publish_with_no_subscribers_is_noop() -> None:
    Broadcaster().publish("dropped")  # must not raise


async def test_unsubscribe_removes_queue() -> None:
    broadcaster = Broadcaster()
    with broadcaster.subscribe() as queue:
        broadcaster.publish("in")
        assert await asyncio.wait_for(queue.get(), 1) == "in"
    broadcaster.publish("out")  # queue is unsubscribed
    assert queue.empty()


async def test_publish_event_serializes_transcript_event(tmp_path: Path) -> None:
    broadcaster = Broadcaster()
    meta = MeetingMeta(
        id=uuid4(), title="t", started_at=datetime(2026, 6, 26, tzinfo=UTC), folder=tmp_path
    )
    session = MeetingSession(meta=meta, capture=_DummyCapture(), broadcaster=broadcaster)
    with broadcaster.subscribe() as queue:
        session.publish_event(
            TranscriptEvent(
                kind="final",
                stream=Stream.ME,
                speaker_label="Me",
                text="hello there",
                start_s=0.0,
                end_s=1.5,
            )
        )
        message = json.loads(await asyncio.wait_for(queue.get(), 1))
    assert message == {
        "kind": "final",
        "stream": "me",
        "speaker_label": "Me",
        "text": "hello there",
        "start_s": 0.0,
        "end_s": 1.5,
    }
