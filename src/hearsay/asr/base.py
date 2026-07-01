"""ASR backend seam.

The post-meeting refine re-transcribes each diarizer turn through an :class:`ASRBackend`
(FluidAudio Parakeet on the ANE, via the ``hearsay-asr`` sidecar). ``transcribe`` is
synchronous; the refine runs it off the event loop with ``asyncio.to_thread``.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from typing import Protocol, runtime_checkable

SAMPLE_RATE = 16_000


@dataclass(frozen=True, slots=True)
class ASRSegment:
    """A transcribed span, in seconds relative to the start of the input clip."""

    text: str
    start_s: float
    end_s: float


@runtime_checkable
class ASRBackend(Protocol):
    @property
    def name(self) -> str: ...

    @property
    def model(self) -> str: ...

    def transcribe(self, samples: Sequence[float]) -> list[ASRSegment]:
        """Transcribe 16 kHz mono float samples in [-1, 1] into ordered segments."""
        ...

    def close(self) -> None:
        """Release backend resources (e.g. a sidecar process). No-op for in-process backends."""
        ...
