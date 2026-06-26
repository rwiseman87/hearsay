"""The ``identities`` table: a cross-meeting person.

Someone the user has named. A diarization :class:`~hearsay.models.cluster.Cluster`
in any meeting binds to an identity (manual rename in Phase 2; roster/voiceprint
auto-binding later), so a name given in one meeting can be suggested in the next.
"""

from __future__ import annotations

from sqlalchemy import String
from sqlalchemy.orm import Mapped, mapped_column

from hearsay.models.base import Base


class Identity(Base):
    __tablename__ = "identities"

    # Unique so a name resolves to one person: the rename flow gets-or-creates by
    # name, and past identities are offered as suggestions for the next meeting.
    display_name: Mapped[str] = mapped_column(String(255), unique=True)
    email: Mapped[str | None] = mapped_column(String(255), default=None)
