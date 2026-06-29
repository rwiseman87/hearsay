"""Stream the Them PCM to a WAV for post-meeting re-diarization.

Recorded only when ``diarization.refine`` is enabled (raw audio is otherwise not
retained). The meeting-time offset of the first sample is captured so a later pyannote
pass can map its turn times back onto segment timestamps -- both share the host_ts clock.
"""

from __future__ import annotations

import json
import wave
from array import array
from collections.abc import Sequence
from pathlib import Path

SAMPLE_RATE = 16_000


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
            # pyannote turn times onto segment timestamps.
            sidecar = self._path.with_suffix(".json")
            sidecar.write_text(
                json.dumps(
                    {"start_offset_s": self._start_offset_s, "sample_rate": self._sample_rate}
                ),
                encoding="utf-8",
            )
