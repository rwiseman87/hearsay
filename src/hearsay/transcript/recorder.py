"""Record meeting audio to WAV.

Two recorders, both fed the pipeline's PCM chunks (float in [-1, 1], stamped with a
meeting-relative ``t0_s`` on the shared host_ts clock):

- :class:`ThemAudioRecorder` -- the Them track only, for post-meeting re-diarization; recorded
  when ``diarization.refine`` is on. The first sample's offset is captured so the diarizer's turn
  times map back onto segment timestamps.
- :class:`MeetingAudioRecorder` -- a single timeline-accurate mixed (Me+Them) track, for
  in-browser playback; recorded when ``audio.record`` is on. Sample N is meeting time N/rate, so
  the UI maps a segment's ``start_s`` straight onto ``audio.currentTime``.
"""

from __future__ import annotations

import json
import wave
from array import array
from collections.abc import Sequence
from pathlib import Path

import numpy as np

from hearsay.enums import Stream

SAMPLE_RATE = 16_000
# Capture levels run low (mic/system-audio peaks are often well under half scale), so a raw mix
# plays back faint and, turned up, noisy. Normalize the playback mix so its peak reaches this
# fraction of full scale; skip when the whole take is essentially silent (don't amplify noise).
_PLAYBACK_PEAK = 0.9
_MIN_PEAK_TO_NORMALIZE = 1e-3
# Per-chunk host_ts jitters a few ms, so placing every chunk by its own t0_s scatters silent gaps
# through the mix (choppy playback). Write each stream contiguously and only re-anchor to t0_s when
# it diverges past this -- i.e. a genuine delivery gap (e.g. a tap rebuild), not clock jitter.
_RESYNC_GAP = SAMPLE_RATE // 5  # 0.2 s


def read_offset_s(wav_path: Path) -> float:
    """Read the recorded meeting-time offset sidecar (``0.0`` if absent)."""
    sidecar = wav_path.with_suffix(".json")
    if not sidecar.exists():
        return 0.0
    data = json.loads(sidecar.read_text(encoding="utf-8"))
    return float(data.get("start_offset_s", 0.0))


class ThemAudioRecorder:
    """Streaming 16 kHz mono WAV writer for the Them track (opened on first write)."""

    def __init__(self, path: Path, *, sample_rate: int = SAMPLE_RATE) -> None:
        self._path = path
        self._sample_rate = sample_rate
        self._wav: wave.Wave_write | None = None
        self._start_offset_s: float | None = None

    @property
    def path(self) -> Path:
        return self._path

    @property
    def start_offset_s(self) -> float | None:
        """Meeting time (s) of the WAV's first sample; ``None`` if nothing was recorded."""
        return self._start_offset_s

    def write(self, samples: Sequence[float], *, t0_s: float) -> None:
        """Append ``samples`` (float in [-1, 1]) captured at meeting time ``t0_s``."""
        if self._wav is None:
            self._path.parent.mkdir(parents=True, exist_ok=True)
            # Streaming handle: held open across chunks, closed at meeting end (no `with`).
            self._wav = wave.open(str(self._path), "wb")  # noqa: SIM115
            self._wav.setnchannels(1)
            self._wav.setsampwidth(2)
            self._wav.setframerate(self._sample_rate)
            self._start_offset_s = t0_s
        pcm = array("h", (max(-32768, min(32767, round(s * 32767))) for s in samples))
        self._wav.writeframes(pcm.tobytes())

    def close(self) -> None:
        if self._wav is not None:
            self._wav.close()
            self._wav = None
            # Persist the meeting-time offset so an out-of-process `rediarize` can align
            # diarizer turn times onto segment timestamps.
            sidecar = self._path.with_suffix(".json")
            sidecar.write_text(
                json.dumps(
                    {"start_offset_s": self._start_offset_s, "sample_rate": self._sample_rate}
                ),
                encoding="utf-8",
            )


class MeetingAudioRecorder:
    """Accumulates one timeline-accurate mixed (Me+Them) 16 kHz mono WAV for playback.

    Fed both streams' chunks by meeting time: each chunk is placed at sample ``round(t0_s * rate)``
    and summed where the streams overlap, so the WAV's sample N is meeting time N/rate (gaps and
    the leading offset are silence). Held in memory during the meeting (a growth-amortized buffer)
    and written once at :meth:`close`; a multi-hour meeting holds a proportional buffer.
    """

    def __init__(self, path: Path, *, sample_rate: int = SAMPLE_RATE) -> None:
        self._path = path
        self._sample_rate = sample_rate
        self._buf: np.ndarray | None = None  # float32 mix buffer (capacity >= _len)
        self._len = 0  # logical length (samples written so far, by meeting time)
        self._cursor: dict[Stream, int] = {}  # next contiguous write position per stream

    @property
    def path(self) -> Path:
        return self._path

    def _ensure(self, size: int) -> None:
        if self._buf is None:
            self._buf = np.zeros(max(size, self._sample_rate), dtype=np.float32)
        elif size > self._buf.size:
            capacity = self._buf.size
            while capacity < size:
                capacity *= 2
            grown = np.zeros(capacity, dtype=np.float32)
            grown[: self._len] = self._buf[: self._len]
            self._buf = grown

    def write(self, samples: Sequence[float], *, t0_s: float, stream: Stream) -> None:
        """Append ``samples`` (float in [-1, 1]) for ``stream``, mixing it into the buffer.

        Each stream is written contiguously from its first sample's meeting time; a chunk only
        jumps to ``round(t0_s * rate)`` when that diverges from the running cursor by more than
        ``_RESYNC_GAP`` (a real delivery gap), so per-chunk clock jitter never punches holes.
        """
        chunk = np.asarray(samples, dtype=np.float32)
        if chunk.size == 0:
            return
        target = max(0, round(t0_s * self._sample_rate))
        cursor = self._cursor.get(stream)
        start = target if cursor is None or abs(target - cursor) > _RESYNC_GAP else cursor
        end = start + chunk.size
        self._ensure(end)
        assert self._buf is not None
        self._buf[start:end] += chunk  # sum where Me and Them overlap
        self._cursor[stream] = end
        self._len = max(self._len, end)

    def close(self) -> None:
        if self._buf is None or self._len == 0:
            return
        mix = self._buf[: self._len]
        peak = float(np.max(np.abs(mix)))
        if peak > _MIN_PEAK_TO_NORMALIZE:  # lift the quiet capture up to a healthy playback level
            mix = mix * (_PLAYBACK_PEAK / peak)
        pcm = np.round(np.clip(mix, -1.0, 1.0) * 32767.0).astype(np.int16)
        self._path.parent.mkdir(parents=True, exist_ok=True)
        with wave.open(str(self._path), "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(self._sample_rate)
            wav.writeframes(pcm.tobytes())
