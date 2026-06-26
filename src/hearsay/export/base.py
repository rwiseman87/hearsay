"""Output-sink seam.

Transcript output is written through :class:`TranscriptSink` so a future central /
object-store sink can replace local markdown without touching the pipeline. Only
**final** segments are written (partials are UI-only).
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Protocol
from uuid import UUID


@dataclass(frozen=True, slots=True)
class MeetingMeta:
    id: UUID
    title: str
    started_at: datetime
    folder: Path


@dataclass(frozen=True, slots=True)
class TranscriptLine:
    """A finalized segment to render. Times are meeting-relative seconds."""

    speaker_label: str
    text: str
    start_s: float
    end_s: float


class TranscriptSink(Protocol):
    async def open(self, meeting: MeetingMeta) -> None:
        """Create the meeting folder + metadata and ready the transcript."""
        ...

    async def append(self, line: TranscriptLine) -> None:
        """Append one finalized segment live (append-order; may interleave streams)."""
        ...

    async def finalize(
        self, lines: list[TranscriptLine], *, ended_at: datetime, status: str
    ) -> None:
        """Atomically rewrite the transcript from ``lines`` (sorted) and finalize metadata."""
        ...
