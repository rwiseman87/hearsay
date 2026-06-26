"""Async media channel over the connected ``media.sock`` stream.

Parses 28-byte header + payload frames and routes audio into per-stream queues,
tracking ``seq`` gaps (dropped frames). A ``None`` queue item marks end-of-stream
(an ``eos`` frame or socket EOF).
"""

from __future__ import annotations

import asyncio
import contextlib
from dataclasses import dataclass

from hearsay.enums import FrameType, Stream
from hearsay.helper.protocol import (
    HEADER_SIZE,
    MediaFrame,
    audio_samples,
    decode,
    expected_payload_len,
)
from hearsay.log import get_logger


@dataclass(frozen=True, slots=True)
class AudioChunk:
    """One decoded audio frame's worth of samples, stamped with its ``host_ts``."""

    host_ts: int
    samples: tuple[float, ...]


@dataclass(slots=True)
class StreamStats:
    frames: int = 0
    samples: int = 0
    dropped: int = 0
    ended: bool = False


class MediaChannel:
    def __init__(self, reader: asyncio.StreamReader) -> None:
        self._reader = reader
        self.queues: dict[Stream, asyncio.Queue[AudioChunk | None]] = {
            Stream.ME: asyncio.Queue(),
            Stream.THEM: asyncio.Queue(),
        }
        self.stats: dict[Stream, StreamStats] = {
            Stream.ME: StreamStats(),
            Stream.THEM: StreamStats(),
        }
        self._expected: dict[Stream, int] = {}
        self._log = get_logger("hearsay.media")
        self._task: asyncio.Task[None] | None = None

    def start(self) -> None:
        """Begin the background frame pump (idempotent)."""
        if self._task is None:
            self._task = asyncio.create_task(self._pump())

    async def _pump(self) -> None:
        while True:
            frame = await self._read_frame()
            if frame is None:
                break  # EOF
            self._track_seq(frame)
            if frame.type is FrameType.AUDIO:
                st = self.stats[frame.stream]
                st.frames += 1
                st.samples += frame.n_samples
                await self.queues[frame.stream].put(
                    AudioChunk(host_ts=frame.host_ts, samples=audio_samples(frame))
                )
            elif frame.type is FrameType.EOS:
                self.stats[frame.stream].ended = True
                await self.queues[frame.stream].put(None)
        # On EOF, release any consumer still waiting on a stream that never sent eos.
        for stream, queue in self.queues.items():
            if not self.stats[stream].ended:
                self.stats[stream].ended = True
                await queue.put(None)

    def _track_seq(self, frame: MediaFrame) -> None:
        expected = self._expected.get(frame.stream)
        if expected is not None and frame.seq > expected:
            gap = frame.seq - expected
            self.stats[frame.stream].dropped += gap
            self._log.warning(
                "media seq gap on %s: expected %d, got %d (%d dropped)",
                frame.stream,
                expected,
                frame.seq,
                gap,
            )
        self._expected[frame.stream] = frame.seq + 1

    async def _read_frame(self) -> MediaFrame | None:
        try:
            header = await self._reader.readexactly(HEADER_SIZE)
        except asyncio.IncompleteReadError:
            return None
        payload_len = expected_payload_len(header)
        payload = b""
        if payload_len:
            try:
                payload = await self._reader.readexactly(payload_len)
            except asyncio.IncompleteReadError:
                return None
        return decode(header + payload)

    async def aclose(self) -> None:
        if self._task is not None:
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._task
            self._task = None
