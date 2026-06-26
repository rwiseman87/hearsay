"""Output sinks for transcript + notes (local markdown now; central later)."""

from __future__ import annotations

from hearsay.export.base import MeetingMeta, TranscriptLine, TranscriptSink
from hearsay.export.local_markdown import LocalMarkdownSink

__all__ = ["LocalMarkdownSink", "MeetingMeta", "TranscriptLine", "TranscriptSink"]
