"""ASR backend seam.

One concrete backend ships per install extra (whisper.cpp by default, mlx opt-in),
all behind :class:`ASRBackend` so the active model/backend is swappable from config.
``transcribe`` is synchronous and CPU/GPU-bound; the pipeline runs it off the event
loop with ``asyncio.to_thread``.
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

    def transcribe(
        self, samples: Sequence[float], *, language: str | None = None
    ) -> list[ASRSegment]:
        """Transcribe 16 kHz mono float samples in [-1, 1] into ordered segments."""
        ...
