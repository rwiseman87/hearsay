"""The ``meeting_assets`` table: a manifest of a meeting's on-disk artifacts.

One row per file written for the meeting (transcript, metadata, recorded audio). Tracking
them in the DB lets a meeting's storage be re-pointed and validated without guessing which
files it has -- whether ``audio.wav`` exists depends on the audio-record setting at capture
time, which the current setting can't tell us. ``rel_path`` is relative to the meeting folder;
the absolute location is the meeting's ``storage_root``.
"""

from __future__ import annotations

import uuid
from typing import TYPE_CHECKING

from sqlalchemy import BigInteger, ForeignKey, String, UniqueConstraint, Uuid
from sqlalchemy.orm import Mapped, mapped_column, relationship

from hearsay.enums import AssetKind
from hearsay.models.base import Base, str_enum

if TYPE_CHECKING:
    from hearsay.models.meeting import Meeting


class MeetingAsset(Base):
    __tablename__ = "meeting_assets"
    __table_args__ = (
        UniqueConstraint("meeting_id", "rel_path", name="uq_meeting_assets_meeting_rel_path"),
    )

    meeting_id: Mapped[uuid.UUID] = mapped_column(
        Uuid(), ForeignKey("meetings.id", ondelete="CASCADE")
    )
    kind: Mapped[AssetKind] = mapped_column(str_enum(AssetKind))
    # Path relative to the meeting folder (e.g. "audio.wav"), joined with the meeting dir.
    rel_path: Mapped[str] = mapped_column(String(512))
    # File size in bytes at the last manifest sync.
    size_bytes: Mapped[int] = mapped_column(BigInteger)

    meeting: Mapped[Meeting] = relationship(back_populates="assets")
