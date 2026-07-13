from __future__ import annotations

from sqlalchemy import delete, select, text
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import selectinload

from hearsay.enums import MeetingStatus, Stream
from hearsay.models import Meeting, Segment


async def test_meeting_roundtrip_sets_uuid_and_timestamps(session: AsyncSession) -> None:
    meeting = Meeting(title="Standup", folder="2026-06-26_0900_standup", storage_root="/out")
    session.add(meeting)
    await session.commit()

    assert meeting.id is not None
    assert meeting.created_at is not None
    assert meeting.updated_at is not None
    assert meeting.status is MeetingStatus.RECORDING  # default
    assert meeting.ended_at is None


async def test_segments_ordered_by_start(session: AsyncSession) -> None:
    meeting = Meeting(title="Sync", folder="2026-06-26_1000_sync", storage_root="/out")
    meeting.segments = [
        Segment(stream=Stream.THEM, speaker_label="Them", text="second", start_s=2.0, end_s=3.0),
        Segment(stream=Stream.ME, speaker_label="Me", text="first", start_s=0.0, end_s=1.0),
    ]
    session.add(meeting)
    await session.commit()
    meeting_id = meeting.id  # capture before expiring (avoids a sync lazy reload)
    # Drop the in-memory collection (kept by expire_on_commit=False) so the
    # re-query reloads from the DB and the relationship's order_by takes effect.
    session.expire_all()

    loaded = await session.scalar(
        select(Meeting).where(Meeting.id == meeting_id).options(selectinload(Meeting.segments))
    )
    assert loaded is not None
    assert [s.text for s in loaded.segments] == ["first", "second"]
    assert loaded.segments[0].stream is Stream.ME


async def test_enum_columns_persist_values_not_names(session: AsyncSession) -> None:
    meeting = Meeting(title="X", folder="f", storage_root="/out")
    meeting.segments = [
        Segment(stream=Stream.THEM, speaker_label="Them", text="hi", start_s=0.0, end_s=1.0)
    ]
    session.add(meeting)
    await session.commit()

    status = await session.scalar(
        text("SELECT status FROM meetings WHERE id = :id").bindparams(id=meeting.id)
    )
    stream = await session.scalar(
        text("SELECT stream FROM segments WHERE meeting_id = :id").bindparams(id=meeting.id)
    )
    assert status == "recording"
    assert stream == "them"


async def test_delete_meeting_cascades_to_segments(session: AsyncSession) -> None:
    meeting = Meeting(title="Y", folder="g", storage_root="/out")
    meeting.segments = [
        Segment(stream=Stream.ME, speaker_label="Me", text="a", start_s=0.0, end_s=1.0)
    ]
    session.add(meeting)
    await session.commit()

    # Core DELETE (not session.delete) so the row is removed without the ORM
    # walking the relationship — this exercises the DB-level ON DELETE CASCADE,
    # which only fires because the engine sets PRAGMA foreign_keys=ON.
    await session.execute(delete(Meeting).where(Meeting.id == meeting.id))
    await session.commit()

    remaining = (await session.scalars(select(Segment))).all()
    assert remaining == []
