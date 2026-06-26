"""Voice-activity detection seam + the streaming segmenter.

A :class:`VAD` scores fixed-size frames; :class:`Segmenter` turns a stream of those
scores into utterances using start/stop hysteresis (``min_speech_ms`` /
``min_silence_ms``) and emits periodic non-final snapshots for live partials. The
segmenter is pure and is the high-value unit-test surface; the real Silero VAD lives
behind this protocol so CI can exercise segmentation with a deterministic stub.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from typing import Protocol, runtime_checkable

SAMPLE_RATE = 16_000


@runtime_checkable
class VAD(Protocol):
    @property
    def frame_samples(self) -> int:
        """Frame size this VAD scores (e.g. 512 samples = 32 ms at 16 kHz)."""
        ...

    def reset(self) -> None:
        """Clear any recurrent state between meetings/streams."""
        ...

    def speech_prob(self, frame: Sequence[float]) -> float:
        """Probability in [0, 1] that ``frame`` (``frame_samples`` long) is speech."""
        ...


@dataclass(frozen=True, slots=True)
class Utterance:
    """A detected speech span. ``is_final`` distinguishes a closed utterance (persist +
    transcript) from an in-progress snapshot (live partial, UI only)."""

    start_s: float
    end_s: float
    samples: tuple[float, ...]
    is_final: bool


class Segmenter:
    def __init__(
        self,
        vad: VAD,
        *,
        threshold: float = 0.5,
        min_speech_ms: int = 250,
        min_silence_ms: int = 600,
        partial_ms: int = 800,
        sample_rate: int = SAMPLE_RATE,
    ) -> None:
        self._vad = vad
        self._threshold = threshold
        self._min_speech_ms = min_speech_ms
        self._min_silence_ms = min_silence_ms
        self._partial_ms = partial_ms
        self._sample_rate = sample_rate
        self._frame_ms = vad.frame_samples / sample_rate * 1000.0
        self._buffer: list[float] = []
        self._next_t = 0.0
        self._reset_utterance()
        self._candidate: list[float] = []
        self._candidate_ms = 0.0
        self._candidate_start_t: float | None = None

    def _reset_utterance(self) -> None:
        self._in_speech = False
        self._utterance: list[float] = []
        self._utterance_start_s = 0.0
        self._silence_ms = 0.0
        self._speech_since_partial_ms = 0.0

    def push(self, samples: Sequence[float], *, t0_s: float) -> list[Utterance]:
        """Feed contiguous 16 kHz samples starting at meeting time ``t0_s``."""
        if not self._buffer:
            self._next_t = t0_s
        self._buffer.extend(samples)
        frame_samples = self._vad.frame_samples
        events: list[Utterance] = []
        while len(self._buffer) >= frame_samples:
            frame = self._buffer[:frame_samples]
            del self._buffer[:frame_samples]
            frame_t = self._next_t
            self._next_t += frame_samples / self._sample_rate
            events.extend(self._consume_frame(frame, frame_t))
        return events

    def _consume_frame(self, frame: list[float], frame_t: float) -> list[Utterance]:
        is_speech = self._vad.speech_prob(frame) >= self._threshold
        frame_end_t = frame_t + len(frame) / self._sample_rate
        if not self._in_speech:
            self._scan_for_start(frame, frame_t, is_speech=is_speech)
            return []
        return self._advance_utterance(frame, frame_end_t, is_speech=is_speech)

    def _scan_for_start(self, frame: list[float], frame_t: float, *, is_speech: bool) -> None:
        if not is_speech:
            self._candidate = []
            self._candidate_ms = 0.0
            self._candidate_start_t = None
            return
        if self._candidate_start_t is None:
            self._candidate_start_t = frame_t
        self._candidate.extend(frame)
        self._candidate_ms += self._frame_ms
        if self._candidate_ms >= self._min_speech_ms:
            # Confirmed start: keep the candidate frames as speech-onset pre-roll.
            self._in_speech = True
            self._utterance = self._candidate
            self._utterance_start_s = self._candidate_start_t
            self._silence_ms = 0.0
            self._speech_since_partial_ms = 0.0
            self._candidate = []
            self._candidate_ms = 0.0
            self._candidate_start_t = None

    def _advance_utterance(
        self, frame: list[float], frame_end_t: float, *, is_speech: bool
    ) -> list[Utterance]:
        self._utterance.extend(frame)
        if is_speech:
            self._silence_ms = 0.0
            self._speech_since_partial_ms += self._frame_ms
            if self._partial_ms > 0 and self._speech_since_partial_ms >= self._partial_ms:
                self._speech_since_partial_ms = 0.0
                return [self._snapshot(frame_end_t, is_final=False)]
            return []
        self._silence_ms += self._frame_ms
        if self._silence_ms >= self._min_silence_ms:
            final = self._snapshot(frame_end_t, is_final=True)
            self._reset_utterance()
            return [final]
        return []

    def _snapshot(self, end_s: float, *, is_final: bool) -> Utterance:
        return Utterance(
            start_s=self._utterance_start_s,
            end_s=end_s,
            samples=tuple(self._utterance),
            is_final=is_final,
        )

    def flush(self) -> Utterance | None:
        """End-of-stream: close any in-progress utterance as final."""
        if self._in_speech and self._utterance:
            final = self._snapshot(self._next_t, is_final=True)
            self._reset_utterance()
            return final
        self._reset_utterance()
        return None
