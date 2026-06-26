"""Local markdown sink: per-meeting folder with meeting.json + transcript.md.

Corruption-safety: a single writer appends only complete, newline-terminated blocks
(write + flush per block), so a crash loses at most the last block, never a partial
line. ``meeting.json`` is written atomically (temp + ``os.replace``). Consecutive
same-speaker segments group under one ``### HH:MM:SS — Speaker`` header.
"""

from __future__ import annotations

import asyncio
import json
import os
from datetime import datetime
from pathlib import Path

from hearsay.enums import MeetingStatus
from hearsay.export.base import MeetingMeta, TranscriptLine
from hearsay.log import get_logger

_log = get_logger("hearsay.export")


def _hhmmss(seconds: float) -> str:
    total = max(0, int(seconds))
    return f"{total // 3600:02d}:{(total % 3600) // 60:02d}:{total % 60:02d}"


class LocalMarkdownSink:
    def __init__(self) -> None:
        self._meeting: MeetingMeta | None = None
        self._transcript_path: Path | None = None
        self._meta_path: Path | None = None
        self._last_speaker: str | None = None
        self._lock = asyncio.Lock()

    @property
    def transcript_path(self) -> Path | None:
        return self._transcript_path

    async def open(self, meeting: MeetingMeta) -> None:
        self._meeting = meeting
        self._transcript_path = meeting.folder / "transcript.md"
        self._meta_path = meeting.folder / "meeting.json"
        self._last_speaker = None
        await asyncio.to_thread(self._open_sync, meeting)

    def _open_sync(self, meeting: MeetingMeta) -> None:
        meeting.folder.mkdir(parents=True, exist_ok=True)
        assert self._transcript_path is not None
        if not self._transcript_path.exists():
            self._transcript_path.write_text(f"# {meeting.title}\n\n", encoding="utf-8")
        self._write_meta(status=MeetingStatus.RECORDING, ended_at=None)

    async def append(self, line: TranscriptLine) -> None:
        async with self._lock:
            await asyncio.to_thread(self._append_sync, line)

    def _append_sync(self, line: TranscriptLine) -> None:
        assert self._transcript_path is not None
        block = ""
        if line.speaker_label != self._last_speaker:
            block += f"\n### {_hhmmss(line.start_s)} — {line.speaker_label}\n\n"
            self._last_speaker = line.speaker_label
        block += f"{line.text}\n"
        with self._transcript_path.open("a", encoding="utf-8") as handle:
            handle.write(block)
            handle.flush()
            os.fsync(handle.fileno())

    async def finalize(
        self, lines: list[TranscriptLine], *, ended_at: datetime, status: str
    ) -> None:
        async with self._lock:
            await asyncio.to_thread(self._finalize_sync, lines, ended_at, status)

    def _finalize_sync(self, lines: list[TranscriptLine], ended_at: datetime, status: str) -> None:
        assert self._meeting is not None and self._transcript_path is not None
        rendered = self._render(lines)
        tmp = self._transcript_path.with_suffix(".md.tmp")
        tmp.write_text(rendered, encoding="utf-8")
        os.replace(tmp, self._transcript_path)
        self._write_meta(status, ended_at)

    def _render(self, lines: list[TranscriptLine]) -> str:
        assert self._meeting is not None
        parts = [f"# {self._meeting.title}\n"]
        last_speaker: str | None = None
        for line in lines:
            if line.speaker_label != last_speaker:
                parts.append(f"\n### {_hhmmss(line.start_s)} — {line.speaker_label}\n")
                last_speaker = line.speaker_label
            parts.append(line.text)
        return "\n".join(parts) + "\n"

    def _write_meta(self, status: str, ended_at: datetime | None) -> None:
        assert self._meeting is not None and self._meta_path is not None
        payload = {
            "id": str(self._meeting.id),
            "title": self._meeting.title,
            "folder": self._meeting.folder.name,
            "status": status,
            "started_at": self._meeting.started_at.isoformat(),
            "ended_at": ended_at.isoformat() if ended_at is not None else None,
        }
        tmp = self._meta_path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        os.replace(tmp, self._meta_path)
