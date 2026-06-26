from __future__ import annotations

from collections.abc import Sequence

from hearsay.vad import Segmenter

FRAME = 160  # 10 ms at 16 kHz, for clean timing arithmetic


class StubVAD:
    """Deterministic VAD: a frame is speech iff any sample is non-zero."""

    def __init__(self, frame_samples: int = FRAME) -> None:
        self._frame_samples = frame_samples
        self.resets = 0

    @property
    def frame_samples(self) -> int:
        return self._frame_samples

    def reset(self) -> None:
        self.resets += 1

    def speech_prob(self, frame: Sequence[float]) -> float:
        return 1.0 if any(sample != 0.0 for sample in frame) else 0.0


def speech(frames: int) -> list[float]:
    return [0.5] * (frames * FRAME)


def silence(frames: int) -> list[float]:
    return [0.0] * (frames * FRAME)


def _segmenter(**kwargs: int) -> Segmenter:
    defaults = {"min_speech_ms": 20, "min_silence_ms": 40, "partial_ms": 0}
    defaults.update(kwargs)
    return Segmenter(StubVAD(), **defaults)  # type: ignore[arg-type]


def test_single_utterance_boundaries() -> None:
    seg = _segmenter()
    # 3 frames silence, 5 speech, 5 silence -> one final utterance.
    events = seg.push(silence(3) + speech(5) + silence(5), t0_s=0.0)
    assert len(events) == 1
    final = events[0]
    assert final.is_final
    # Onset confirmed after 2 speech frames (min_speech 20ms): start at frame 3 (0.03s).
    assert abs(final.start_s - 0.03) < 1e-9
    # Finalizes once trailing silence reaches 40ms (4th silence frame), end at frame 12.
    assert abs(final.end_s - 0.12) < 1e-9
    # Frames 3..11 inclusive (onset pre-roll + speech + trailing silence) = 9 frames.
    assert len(final.samples) == 9 * FRAME


def test_short_blip_below_min_speech_is_ignored() -> None:
    seg = _segmenter()
    events = seg.push(silence(2) + speech(1) + silence(5), t0_s=0.0)
    assert events == []
    assert seg.flush() is None


def test_partials_emitted_on_cadence_then_final_on_flush() -> None:
    seg = _segmenter(min_speech_ms=20, min_silence_ms=10_000, partial_ms=20)
    events = seg.push(speech(10), t0_s=0.0)
    assert all(not e.is_final for e in events)
    # After a 2-frame onset, a partial every 2 speech frames over the remaining 8 -> 4.
    assert len(events) == 4
    final = seg.flush()
    assert final is not None
    assert final.is_final
    assert abs(final.start_s - 0.0) < 1e-9


def test_flush_without_speech_returns_none() -> None:
    seg = _segmenter()
    assert seg.push(silence(5), t0_s=0.0) == []
    assert seg.flush() is None


def test_times_track_push_offset() -> None:
    seg = _segmenter()
    # Stream starts at meeting time 12.0s; boundaries must be absolute, not push-local.
    events = seg.push(silence(2) + speech(5) + silence(5), t0_s=12.0)
    assert len(events) == 1
    assert abs(events[0].start_s - 12.02) < 1e-9


def test_reanchors_to_host_ts_across_chunks() -> None:
    # The segmenter must follow each chunk's host_ts (t0_s), not free-run on sample
    # count: a stream with delivery gaps (e.g. system audio during silence) would
    # otherwise drift behind the other stream. First chunk leaves an 80-sample
    # (0.005s) partial frame buffered; the next chunk's clock jumps to 100s.
    seg = _segmenter()
    seg.push(silence(2) + speech(5) + silence(5) + [0.0] * 80, t0_s=0.0)
    events = seg.push(speech(5) + silence(5), t0_s=100.0)
    assert len(events) == 1
    # start_s tracks the new host_ts (~100s), not the ~0.13s of samples processed.
    assert 99.9 < events[0].start_s < 100.1
