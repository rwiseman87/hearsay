"""The ``clusters`` table: one diarization speaker within a meeting.

The Them stream is clustered into "Speaker 1..N" turns (Me is the mic channel and is
never diarized). ``ordinal`` is the first-appearance number shown as "Speaker N"; a
cluster binds to a cross-meeting :class:`~hearsay.models.identity.Identity` when the
user renames it, and ``locked`` marks that manual binding so later automatic votes
(Phase 3 hints) cannot override it.
"""

from __future__ import annotations

import uuid
from typing import TYPE_CHECKING

from sqlalchemy import Boolean, ForeignKey, Integer, LargeBinary, UniqueConstraint, Uuid
from sqlalchemy.orm import Mapped, mapped_column, relationship

from hearsay.models.base import Base

if TYPE_CHECKING:
    from hearsay.models.identity import Identity
    from hearsay.models.meeting import Meeting


class Cluster(Base):
    __tablename__ = "clusters"
    __table_args__ = (
        UniqueConstraint("meeting_id", "ordinal", name="uq_clusters_meeting_ordinal"),
    )

    meeting_id: Mapped[uuid.UUID] = mapped_column(
        Uuid(), ForeignKey("meetings.id", ondelete="CASCADE")
    )
    # First-appearance number rendered as "Speaker N" until the cluster is named.
    ordinal: Mapped[int] = mapped_column(Integer)
    identity_id: Mapped[uuid.UUID | None] = mapped_column(
        Uuid(), ForeignKey("identities.id", ondelete="SET NULL"), default=None
    )
    # A manual rename locks the binding so vote-based hints (Phase 3) can't flip it.
    locked: Mapped[bool] = mapped_column(Boolean, default=False)
    # Running mean speaker embedding (float32 vector as raw bytes): the voiceprint
    # that drives online clustering within a meeting and recognition across meetings.
    centroid: Mapped[bytes | None] = mapped_column(LargeBinary, default=None)

    meeting: Mapped[Meeting] = relationship(back_populates="clusters")
    identity: Mapped[Identity | None] = relationship()
