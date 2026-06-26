"""Meeting + segment persistence logic.

Routers stay thin and delegate here. Each method is a single logical write and
commits once; multi-step writes (none yet) would wrap an explicit transaction.
"""

from __future__ import annotations

import re
from datetime import UTC, datetime
from uuid import UUID

from sqlalchemy import func, select
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.enums import MeetingStatus, Stream
from hearsay.models import Meeting, Segment

_SLUG_STRIP = re.compile(r"[^a-z0-9]+")


def slugify(title: str) -> str:
    slug = _SLUG_STRIP.sub("-", title.lower()).strip("-")
    return slug or "meeting"


def meeting_folder_name(title: str, when: datetime) -> str:
    """``<YYYY-MM-DD_HHMM>_<slug>`` — the per-meeting on-disk folder name."""
    return f"{when:%Y-%m-%d_%H%M}_{slugify(title)}"


class MeetingService:
    def __init__(self, session: AsyncSession) -> None:
        self._session = session

    async def create(self, *, title: str, folder: str, started_at: datetime) -> Meeting:
        meeting = Meeting(title=title, folder=folder, started_at=started_at)
        self._session.add(meeting)
        await self._session.commit()
        await self._session.refresh(meeting)
        return meeting

    async def get(self, meeting_id: UUID) -> Meeting | None:
        return await self._session.get(Meeting, meeting_id)

    async def list_meetings(self, *, page: int, page_size: int) -> tuple[list[Meeting], int]:
        total = await self._session.scalar(select(func.count()).select_from(Meeting)) or 0
        rows = await self._session.scalars(
            select(Meeting)
            .order_by(Meeting.started_at.desc())
            .offset((page - 1) * page_size)
            .limit(page_size)
        )
        return list(rows), total

    async def list_segments(
        self, meeting_id: UUID, *, page: int, page_size: int
    ) -> tuple[list[Segment], int]:
        where = Segment.meeting_id == meeting_id
        total = (
            await self._session.scalar(select(func.count()).select_from(Segment).where(where)) or 0
        )
        rows = await self._session.scalars(
            select(Segment)
            .where(where)
            .order_by(Segment.start_s)
            .offset((page - 1) * page_size)
            .limit(page_size)
        )
        return list(rows), total

    async def add_segment(
        self,
        meeting_id: UUID,
        *,
        stream: Stream,
        speaker_label: str,
        text: str,
        start_s: float,
        end_s: float,
    ) -> Segment:
        segment = Segment(
            meeting_id=meeting_id,
            stream=stream,
            speaker_label=speaker_label,
            text=text,
            start_s=start_s,
            end_s=end_s,
        )
        self._session.add(segment)
        await self._session.commit()
        await self._session.refresh(segment)
        return segment

    async def finalize(self, meeting_id: UUID) -> Meeting | None:
        meeting = await self.get(meeting_id)
        if meeting is None:
            return None
        meeting.status = MeetingStatus.FINALIZED
        meeting.ended_at = datetime.now(UTC)
        await self._session.commit()
        await self._session.refresh(meeting)
        return meeting

    async def delete(self, meeting_id: UUID) -> bool:
        meeting = await self.get(meeting_id)
        if meeting is None:
            return False
        await self._session.delete(meeting)
        await self._session.commit()
        return True
