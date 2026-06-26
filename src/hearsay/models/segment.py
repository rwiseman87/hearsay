"""The ``segments`` table: one finalized transcript segment.

Only **final** segments are persisted (partials stream to the UI only). Times are
meeting-relative seconds derived from the helper's single monotonic ``host_ts``
clock, so they align across the Me/Them streams regardless of hardware clocks.
"""

from __future__ import annotations

import uuid
from typing import TYPE_CHECKING

from sqlalchemy import Float, ForeignKey, Index, String, Text, Uuid
from sqlalchemy.orm import Mapped, mapped_column, relationship

from hearsay.enums import Stream
from hearsay.models.base import Base, str_enum

if TYPE_CHECKING:
    from hearsay.models.meeting import Meeting


class Segment(Base):
    __tablename__ = "segments"
    __table_args__ = (Index("ix_segments_meeting_start", "meeting_id", "start_s"),)

    meeting_id: Mapped[uuid.UUID] = mapped_column(
        Uuid(), ForeignKey("meetings.id", ondelete="CASCADE")
    )
    stream: Mapped[Stream] = mapped_column(str_enum(Stream))
    # Resolved display name for the speaker ("Me" / "Them" in Phase 1; "Speaker N"
    # and real identities arrive with diarization + fusion in later phases).
    speaker_label: Mapped[str] = mapped_column(String(64))
    text: Mapped[str] = mapped_column(Text)
    start_s: Mapped[float] = mapped_column(Float)
    end_s: Mapped[float] = mapped_column(Float)

    meeting: Mapped[Meeting] = relationship(back_populates="segments")
