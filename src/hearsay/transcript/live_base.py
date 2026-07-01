"""Shared plumbing for a streaming audio-AI sidecar.

A sidecar (FluidAudio on the ANE) does the VAD/diarization + ASR for one stream; the Python
side just spawns it, streams the stream's PCM in over stdin, and reads the NDJSON segments it
emits on stdout. This base owns that lifecycle (spawn, feed, read loop, drain on close);
subclasses implement :meth:`_handle` to persist + broadcast one emitted segment.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import struct
from collections import deque
from collections.abc import Sequence
from pathlib import Path
from typing import Any
from uuid import UUID

from hearsay.db import Database
from hearsay.export import TranscriptSink
from hearsay.log import get_logger
from hearsay.transcript.broadcast import Broadcaster

_log = get_logger("hearsay.live")


class LiveSidecarProcessor:
    """Owns one streaming sidecar for a meeting: feed its stream's audio, persist its segments."""

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
        self._stderr_reader: asyncio.Task[None] | None = None
        # FluidAudio is chatty on stderr, so buffer it silently and surface only the tail if the
        # sidecar dies (a broken pipe here, or a non-zero exit at close) -- that is our only window
        # into why a sidecar crashed, since stderr is otherwise discarded.
        self._stderr_tail: deque[str] = deque(maxlen=50)
        self._broken = False  # the sidecar's pipe closed mid-meeting -> stop feeding it
        self._offset_s: float | None = None  # meeting time of the first sample fed
        self._write_lock = asyncio.Lock()

    async def start(self) -> None:
        if not self._binary_path.exists():
            _log.warning(
                "%s not found at %s; its live transcription is off",
                self._binary_path.name,
                self._binary_path,
            )
            return
        self._proc = await asyncio.create_subprocess_exec(
            str(self._binary_path),
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        self._reader = asyncio.create_task(self._read_loop())
        self._stderr_reader = asyncio.create_task(self._read_stderr())

    async def feed(self, samples: Sequence[float], t0_s: float) -> None:
        if self._proc is None or self._proc.stdin is None or self._broken:
            return
        if self._offset_s is None:
            self._offset_s = t0_s
        import numpy as np  # noqa: PLC0415 (optional dep; only with the asr extra)

        pcm = np.ascontiguousarray(np.asarray(samples, dtype=np.float32))
        data = struct.pack("<I", len(pcm)) + pcm.tobytes()
        async with self._write_lock:
            try:
                self._proc.stdin.write(data)
                await self._proc.stdin.drain()
            except BrokenPipeError, ConnectionResetError:
                # The sidecar exited/crashed; stop feeding it so a dead pipe never crashes the
                # capture pipeline or the meeting stop. Segments it already emitted are kept, and
                # the post-meeting refine still re-diarizes them.wav.
                self._broken = True
                _log.warning(
                    "%s sidecar pipe closed mid-meeting; live transcription stopped (%s)",
                    self._binary_path.name,
                    "; ".join(list(self._stderr_tail)[-3:]) or "no stderr",
                )

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

    async def _read_stderr(self) -> None:
        assert self._proc is not None and self._proc.stderr is not None
        while True:
            line = await self._proc.stderr.readline()
            if not line:
                break
            self._stderr_tail.append(line.decode("utf-8", "replace").rstrip())

    async def _handle(self, seg: dict[str, Any]) -> None:
        """Persist + broadcast one segment the sidecar emitted (subclass-specific)."""
        raise NotImplementedError

    async def close(self) -> None:
        if self._proc is None:
            return
        if self._proc.stdin is not None:
            with contextlib.suppress(OSError):
                self._proc.stdin.close()  # EOF -> sidecar finalizes its tail then exits
        for reader in (self._reader, self._stderr_reader):
            if reader is None:
                continue
            try:
                await reader  # the stdout reader drains the finalized tail before we return
            except Exception:
                _log.exception("%s reader failed during close", self._binary_path.name)
        try:
            await asyncio.wait_for(self._proc.wait(), timeout=10)
        except TimeoutError:
            self._proc.kill()
        if self._proc.returncode not in (0, None) and not self._broken:
            _log.warning(
                "%s sidecar exited with code %s (%s)",
                self._binary_path.name,
                self._proc.returncode,
                "; ".join(list(self._stderr_tail)[-3:]) or "no stderr",
            )
