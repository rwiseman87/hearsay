"""The ``meetings`` table: one row per recording session."""

from __future__ import annotations

from datetime import datetime

from sqlalchemy import DateTime, String
from sqlalchemy.orm import Mapped, mapped_column, relationship

from hearsay.enums import MeetingStatus
from hearsay.models.base import Base, _utcnow, str_enum
from hearsay.models.cluster import Cluster
from hearsay.models.segment import Segment


class Meeting(Base):
    __tablename__ = "meetings"

    title: Mapped[str] = mapped_column(String(255))
    # Directory name of the on-disk meeting folder (joined with settings.output_dir
    # at read time, so the output root can move without rewriting rows).
    folder: Mapped[str] = mapped_column(String(512))
    status: Mapped[MeetingStatus] = mapped_column(
        str_enum(MeetingStatus), default=MeetingStatus.RECORDING
    )
    started_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=_utcnow)
    ended_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), default=None)

    segments: Mapped[list[Segment]] = relationship(
        back_populates="meeting",
        cascade="all, delete-orphan",
        order_by="Segment.start_s",
        passive_deletes=True,
    )
    clusters: Mapped[list[Cluster]] = relationship(
        back_populates="meeting",
        cascade="all, delete-orphan",
        order_by="Cluster.ordinal",
        passive_deletes=True,
    )
