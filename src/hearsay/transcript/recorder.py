"""Record meeting audio to a single WAV.

:class:`MeetingAudioRecorder` accumulates one timeline-accurate **stereo** track per meeting
(``<folder>/audio.wav``), fed the pipeline's PCM chunks (float in [-1, 1], stamped with a
meeting-relative ``t0_s`` on the shared host_ts clock). Me is the left channel, Them the right,
each placed by meeting time so sample N is meeting time N/rate. This one file serves both:

- **playback** -- the UI plays the stereo file (Me left, Them right; mono devices downmix), and
  maps a segment's ``start_s`` straight onto ``audio.currentTime``.
- **the post-meeting refine** -- it reads the right (Them) channel, which is as clean as a
  Them-only recording (Me and Them are captured as separate devices). Timeline anchoring means a
  turn's time is already absolute meeting time, so no offset sidecar is needed.

Recorded when ``audio.record`` is on.
"""

from __future__ import annotations

import wave
from collections.abc import Sequence
from pathlib import Path
from typing import Any

import numpy as np

from hearsay.enums import Stream

SAMPLE_RATE = 16_000
# Capture levels run low (mic/system-audio peaks are often well under half scale), so a raw mix
# plays back faint and, turned up, noisy. Normalize so the loudest sample across both channels
# reaches this fraction of full scale (preserving the Me/Them balance); skip when the take is
# essentially silent (don't amplify noise). Normalizing by the overall peak keeps every stored
# sample in range, so neither channel nor a mono downmix clips.
_PLAYBACK_PEAK = 0.9
_MIN_PEAK_TO_NORMALIZE = 1e-3
# Per-chunk host_ts jitters a few ms, so placing every chunk by its own t0_s scatters silent gaps
# through a channel (choppy playback). Write each stream contiguously and only re-anchor to t0_s
# when it diverges past this -- i.e. a genuine delivery gap (e.g. a tap rebuild), not clock jitter.
_RESYNC_GAP = SAMPLE_RATE // 5  # 0.2 s

# Left channel = Me, right channel = Them.
_CHANNEL: dict[Stream, int] = {Stream.ME: 0, Stream.THEM: 1}


def read_them_channel(path: Path) -> tuple[Any, int]:
    """Read the Them (right) channel of the stereo ``audio.wav`` as float32 in [-1, 1].

    Returns ``(samples, sample_rate)``. Falls back to a mono file's only channel so an older
    single-channel recording still refines.
    """
    with wave.open(str(path)) as wav:
        sample_rate = wav.getframerate()
        channels = wav.getnchannels()
        frames = wav.readframes(wav.getnframes())
    interleaved = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    them = interleaved[1::channels] if channels > 1 else interleaved
    return np.ascontiguousarray(them), sample_rate


class MeetingAudioRecorder:
    """Accumulates one timeline-accurate stereo (Me=L, Them=R) 16 kHz WAV.

    Each stream's chunks are placed by meeting time into its channel, so sample N is meeting time
    N/rate (gaps and the leading offset are silence). Held in memory during the meeting (a
    growth-amortized buffer) and written once at :meth:`close`; a multi-hour meeting holds a
    proportional buffer.
    """

    def __init__(self, path: Path, *, sample_rate: int = SAMPLE_RATE) -> None:
        self._path = path
        self._sample_rate = sample_rate
        self._buf: np.ndarray | None = None  # float32, shape (2, capacity): row 0 = Me, 1 = Them
        self._len = 0  # logical length (samples written so far, by meeting time)
        self._cursor: dict[Stream, int] = {}  # next contiguous write position per stream

    @property
    def path(self) -> Path:
        return self._path

    def _ensure(self, size: int) -> None:
        if self._buf is None:
            self._buf = np.zeros((2, max(size, self._sample_rate)), dtype=np.float32)
        elif size > self._buf.shape[1]:
            capacity = self._buf.shape[1]
            while capacity < size:
                capacity *= 2
            grown = np.zeros((2, capacity), dtype=np.float32)
            grown[:, : self._len] = self._buf[:, : self._len]
            self._buf = grown

    def write(self, samples: Sequence[float], *, t0_s: float, stream: Stream) -> None:
        """Append ``samples`` (float in [-1, 1]) for ``stream`` into its channel.

        Each stream is written contiguously from its first sample's meeting time; a chunk only
        jumps to ``round(t0_s * rate)`` when that diverges from the running cursor by more than
        ``_RESYNC_GAP`` (a real delivery gap), so per-chunk clock jitter never punches holes.
        """
        chunk = np.asarray(samples, dtype=np.float32)
        if chunk.size == 0:
            return
        channel = _CHANNEL[stream]
        target = max(0, round(t0_s * self._sample_rate))
        cursor = self._cursor.get(stream)
        start = target if cursor is None or abs(target - cursor) > _RESYNC_GAP else cursor
        end = start + chunk.size
        self._ensure(end)
        assert self._buf is not None
        self._buf[channel, start:end] += chunk
        self._cursor[stream] = end
        self._len = max(self._len, end)

    def close(self) -> None:
        if self._buf is None or self._len == 0:
            return
        stereo = self._buf[:, : self._len]
        peak = float(np.max(np.abs(stereo)))
        if peak > _MIN_PEAK_TO_NORMALIZE:  # lift the quiet capture up to a healthy playback level
            stereo = stereo * (_PLAYBACK_PEAK / peak)
        # Interleave (L, R, L, R, ...) for the 2-channel WAV.
        interleaved = np.clip(stereo, -1.0, 1.0).T.reshape(-1)
        pcm = np.round(interleaved * 32767.0).astype(np.int16)
        self._path.parent.mkdir(parents=True, exist_ok=True)
        with wave.open(str(self._path), "wb") as wav:
            wav.setnchannels(2)
            wav.setsampwidth(2)
            wav.setframerate(self._sample_rate)
            wav.writeframes(pcm.tobytes())
