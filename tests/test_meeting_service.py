from __future__ import annotations

from datetime import UTC, datetime, timedelta
from uuid import uuid4

import pytest
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.enums import MeetingStatus, Stream
from hearsay.services import MeetingService, meeting_folder_name, slugify


@pytest.mark.parametrize(
    ("title", "expected"),
    [
        ("Weekly Sync", "weekly-sync"),
        ("  Q3 Planning!!  ", "q3-planning"),
        ("***", "meeting"),
        ("Café déjà", "caf-d-j"),
    ],
)
def test_slugify(title: str, expected: str) -> None:
    assert slugify(title) == expected


def test_meeting_folder_name() -> None:
    when = datetime(2026, 6, 26, 9, 5, tzinfo=UTC)
    assert meeting_folder_name("Stand Up", when) == "2026-06-26_0905_stand-up"


async def test_create_and_get(session: AsyncSession) -> None:
    svc = MeetingService(session)
    started = datetime.now(UTC)
    created = await svc.create(title="Sync", folder="f", storage_root="/out", started_at=started)

    fetched = await svc.get(created.id)
    assert fetched is not None
    assert fetched.title == "Sync"
    assert fetched.status is MeetingStatus.RECORDING


async def test_get_missing_returns_none(session: AsyncSession) -> None:
    assert await MeetingService(session).get(uuid4()) is None


async def test_list_paginates_newest_first(session: AsyncSession) -> None:
    svc = MeetingService(session)
    base = datetime(2026, 6, 26, 9, 0, tzinfo=UTC)
    for i in range(3):
        await svc.create(
            title=f"m{i}",
            folder=f"f{i}",
            storage_root="/out",
            started_at=base + timedelta(minutes=i),
        )

    page1, total = await svc.list_meetings(page=1, page_size=2)
    assert total == 3
    assert [m.title for m in page1] == ["m2", "m1"]

    page2, total = await svc.list_meetings(page=2, page_size=2)
    assert total == 3
    assert [m.title for m in page2] == ["m0"]


async def test_segments_added_and_listed_in_order(session: AsyncSession) -> None:
    svc = MeetingService(session)
    meeting = await svc.create(
        title="Sync", folder="f", storage_root="/out", started_at=datetime.now(UTC)
    )
    await svc.add_segment(
        meeting.id, stream=Stream.THEM, speaker_label="Them", text="b", start_s=2.0, end_s=3.0
    )
    await svc.add_segment(
        meeting.id, stream=Stream.ME, speaker_label="Me", text="a", start_s=0.0, end_s=1.0
    )

    segments, total = await svc.list_segments(meeting.id, page=1, page_size=10)
    assert total == 2
    assert [s.text for s in segments] == ["a", "b"]


async def test_finalize_sets_status_and_end(session: AsyncSession) -> None:
    svc = MeetingService(session)
    meeting = await svc.create(
        title="Sync", folder="f", storage_root="/out", started_at=datetime.now(UTC)
    )

    finalized = await svc.finalize(meeting.id)
    assert finalized is not None
    assert finalized.status is MeetingStatus.FINALIZED
    assert finalized.ended_at is not None


async def test_delete_removes_meeting(session: AsyncSession) -> None:
    svc = MeetingService(session)
    meeting = await svc.create(
        title="Sync", folder="f", storage_root="/out", started_at=datetime.now(UTC)
    )
    await svc.add_segment(
        meeting.id, stream=Stream.ME, speaker_label="Me", text="x", start_s=0.0, end_s=1.0
    )

    assert await svc.delete(meeting.id) is True
    assert await svc.get(meeting.id) is None
    _, total = await svc.list_segments(meeting.id, page=1, page_size=10)
    assert total == 0


async def test_delete_missing_returns_false(session: AsyncSession) -> None:
    assert await MeetingService(session).delete(uuid4()) is False
