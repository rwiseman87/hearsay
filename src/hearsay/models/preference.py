"""The ``preferences`` table: the writable user-settings overlay.

The typed :class:`~hearsay.config.settings.Settings` object is startup/env-driven and read-only,
so it cannot hold values the UI edits at runtime. Each row stores one settings *section* as a JSON
blob (e.g. ``recording -> {"record": false}``); ``SettingsService`` resolves the effective value
as the stored override when present, otherwise the ``Settings`` default.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import JSON, String, UniqueConstraint
from sqlalchemy.orm import Mapped, mapped_column

from hearsay.models.base import Base


class Preference(Base):
    __tablename__ = "preferences"
    __table_args__ = (UniqueConstraint("section", name="uq_preferences_section"),)

    # The settings section this row overrides (e.g. "recording"); one row per section.
    section: Mapped[str] = mapped_column(String(64))
    # The section's stored values as JSON, validated against its Pydantic schema on read/write.
    value: Mapped[dict[str, Any]] = mapped_column(JSON)
