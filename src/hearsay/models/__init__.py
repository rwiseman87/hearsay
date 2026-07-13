"""SQLAlchemy ORM models.

Importing this package registers every table on ``Base.metadata`` so Alembic
autogenerate and ``create_all`` see the full schema.
"""

from __future__ import annotations

from hearsay.models.asset import MeetingAsset
from hearsay.models.base import Base
from hearsay.models.cluster import Cluster
from hearsay.models.identity import Identity
from hearsay.models.meeting import Meeting
from hearsay.models.segment import Segment

__all__ = ["Base", "Cluster", "Identity", "Meeting", "MeetingAsset", "Segment"]
