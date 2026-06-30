"""Live "Them" processing via the Swift ``hearsay-live`` sidecar.

The sidecar (FluidAudio streaming diarization + Parakeet on the ANE) does the diarization,
ASR, and turn assembly; this just streams the Them PCM in and persists + broadcasts the
labeled segments it emits -- no VAD, no online clustering, no fusion. The post-meeting refine
still re-diarizes the whole track for final accuracy + cross-meeting recognition.
"""

from __future__ import annotations

import asyncio
import json
import struct
from collections.abc import Sequence
from pathlib import Path
from typing import Any
from uuid import UUID

from hearsay.db import Database
from hearsay.enums import Stream
from hearsay.export import TranscriptLine, TranscriptSink
from hearsay.log import get_logger
from hearsay.schemas import TranscriptEvent
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript.broadcast import Broadcaster

_log = get_logger("hearsay.live")


class LiveThemProcessor:
    """Owns the ``hearsay-live`` sidecar for a meeting: feed Them audio, persist its turns."""

    def __init__(
        self,
        *,
        binary_path: Path,
        meeting_id: UUID,
        database: Database,
        broadcaster: Broadcaster,
        sink: TranscriptSink,
    ) -> None:
        self._binary_path = binary_path
        self._meeting_id = meeting_id
        self._db = database
        self._broadcaster = broadcaster
        self._sink = sink
        self._proc: asyncio.subprocess.Process | None = None
        self._reader: asyncio.Task[None] | None = None
        self._offset_s: float | None = None  # meeting time of the first Them sample
        self._cluster_ids: dict[int, UUID] = {}
        self._write_lock = asyncio.Lock()

    async def start(self) -> None:
        if not self._binary_path.exists():
            _log.warning("hearsay-live not found at %s; live Them labeling off", self._binary_path)
            return
        self._proc = await asyncio.create_subprocess_exec(
            str(self._binary_path),
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL,
        )
        self._reader = asyncio.create_task(self._read_loop())

    async def feed(self, samples: Sequence[float], t0_s: float) -> None:
        if self._proc is None or self._proc.stdin is None:
            return
        if self._offset_s is None:
            self._offset_s = t0_s
        import numpy as np  # noqa: PLC0415 (optional dep; only with the asr extra)

        pcm = np.ascontiguousarray(np.asarray(samples, dtype=np.float32))
        data = struct.pack("<I", len(pcm)) + pcm.tobytes()
        async with self._write_lock:
            self._proc.stdin.write(data)
            await self._proc.stdin.drain()

    async def _read_loop(self) -> None:
        assert self._proc is not None and self._proc.stdout is not None
        while True:
            line = await self._proc.stdout.readline()
            if not line:
                break
            try:
                seg = json.loads(line)
            except json.JSONDecodeError:
                continue
            await self._handle(seg)

    async def _handle(self, seg: dict[str, Any]) -> None:
        offset = self._offset_s or 0.0
        ordinal = int(seg["speaker"]) + 1  # sidecar speakers are 0-based
        label = f"Speaker {ordinal}"
        text = str(seg["text"])
        start_s = float(seg["start_s"]) + offset
        end_s = float(seg["end_s"]) + offset
        cluster_id = await self._cluster_for(ordinal)
        async with self._db.session() as session:
            await MeetingService(session).add_segment(
                self._meeting_id,
                stream=Stream.THEM,
                speaker_label=label,
                text=text,
                start_s=start_s,
                end_s=end_s,
                cluster_id=cluster_id,
            )
        await self._sink.append(
            TranscriptLine(speaker_label=label, text=text, start_s=start_s, end_s=end_s)
        )
        self._broadcaster.publish(
            TranscriptEvent(
                kind="final",
                stream=Stream.THEM,
                speaker_label=label,
                text=text,
                start_s=start_s,
                end_s=end_s,
            ).model_dump_json()
        )

    async def _cluster_for(self, ordinal: int) -> UUID:
        existing = self._cluster_ids.get(ordinal)
        if existing is not None:
            return existing
        async with self._db.session() as session:
            cluster = await SpeakerService(session).create_cluster(
                self._meeting_id, ordinal=ordinal
            )
        self._cluster_ids[ordinal] = cluster.id
        return cluster.id

    async def close(self) -> None:
        if self._proc is None:
            return
        if self._proc.stdin is not None:
            self._proc.stdin.close()  # EOF -> sidecar finalizes its tail then exits
        if self._reader is not None:
            await self._reader  # drains the finalized tail before we return
        try:
            await asyncio.wait_for(self._proc.wait(), timeout=10)
        except TimeoutError:
            self._proc.kill()
