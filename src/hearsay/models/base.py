"""Declarative base and shared column helpers for all ORM models.

Every model uses a UUID primary key and ``created_at`` / ``updated_at``
timestamps (see CLAUDE.md). The ``Uuid`` and timezone-aware ``DateTime`` types
are portable: SQLite stores them as text today, PostgreSQL maps to native types
later without a model change.
"""

from __future__ import annotations

import uuid
from datetime import UTC, datetime
from enum import StrEnum

from sqlalchemy import DateTime, Enum, Uuid
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column


def _utcnow() -> datetime:
    return datetime.now(UTC)


def str_enum[E: StrEnum](enum_type: type[E]) -> Enum:
    """A portable ``Enum`` column that stores a :class:`StrEnum`'s values.

    ``native_enum=False`` renders as ``VARCHAR`` + ``CHECK`` (works on SQLite and
    PostgreSQL); ``values_callable`` persists the member *values* (``"recording"``)
    rather than their names, matching the StrEnum JSON serialization.
    """
    return Enum(
        enum_type,
        native_enum=False,
        length=32,
        values_callable=lambda enum: [member.value for member in enum],
    )


class Base(DeclarativeBase):
    """Declarative base; every table inherits a UUID PK and audit timestamps."""

    id: Mapped[uuid.UUID] = mapped_column(Uuid(), primary_key=True, default=uuid.uuid4)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=_utcnow)
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow, onupdate=_utcnow
    )
