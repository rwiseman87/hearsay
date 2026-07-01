"""Offline (post-meeting) diarization: whole-track speaker-turn segmentation.

Unlike the live sidecar's streaming labels, an offline diarizer runs over the *entire* Them
track at once, so it can do sliding-window segmentation + global clustering + overlap
handling -- which the streaming path cannot do as well in real time. The backend
wraps FluidAudio's pyannote community-1 CoreML diarizer (run on the ANE in the Swift
helper); the seam keeps it swappable and lets the refine orchestration be unit-tested
with a stub. Each run also returns a per-speaker voiceprint (FluidAudio's mean-of-segments
speaker embedding) the refine stores + matches across meetings.
"""

from __future__ import annotations

import json
import subprocess
import tempfile
import wave
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Protocol, runtime_checkable

import numpy as np

from hearsay.log import get_logger

if TYPE_CHECKING:
    from hearsay.config.settings import Settings

_log = get_logger("hearsay.diarization")


@dataclass(frozen=True, slots=True)
class SpeakerTurn:
    """One contiguous span attributed to a single speaker (seconds, track-relative)."""

    speaker: str
    start_s: float
    end_s: float


@dataclass(frozen=True, slots=True)
class DiarizationResult:
    """A whole-track diarization: speaker turns + each speaker's voiceprint.

    ``embeddings`` maps a diarizer speaker label (the same label used in ``turns``) to its
    mean speaker embedding; it may be empty (a stub, or a model that exposes no embeddings),
    in which case cross-meeting recognition is simply skipped.
    """

    turns: list[SpeakerTurn]
    embeddings: dict[str, list[float]]


def order_speakers(turns: Sequence[SpeakerTurn]) -> dict[str, int]:
    """Map each diarizer speaker label to a 1-based "Speaker N" ordinal by first appearance."""
    ordinal: dict[str, int] = {}
    for turn in sorted(turns, key=lambda t: t.start_s):
        if turn.speaker not in ordinal:
            ordinal[turn.speaker] = len(ordinal) + 1
    return ordinal


def assign_segment_speaker(
    start_s: float, end_s: float, turns: Sequence[SpeakerTurn], *, offset_s: float
) -> str | None:
    """Speaker label whose turn most overlaps segment ``[start_s, end_s]`` (meeting time).

    Turn times are track-relative (WAV sample 0); ``offset_s`` shifts them onto the
    meeting clock the segment timestamps use. ``None`` if no turn overlaps.
    """
    best_label: str | None = None
    best_overlap = 0.0
    for turn in turns:
        overlap = min(end_s, turn.end_s + offset_s) - max(start_s, turn.start_s + offset_s)
        if overlap > best_overlap:
            best_overlap = overlap
            best_label = turn.speaker
    return best_label


@runtime_checkable
class OfflineDiarizer(Protocol):
    def diarize(self, samples: Sequence[float], *, sample_rate: int) -> DiarizationResult:
        """Speaker turns + per-speaker voiceprints over ``samples`` (mono float in [-1, 1])."""
        ...


class FluidAudioDiarizer:
    """FluidAudio's pyannote community-1 CoreML diarizer, run on the ANE via the helper.

    Offline inference lives in the Swift ``hearsay-diarize`` tool (it owns the CoreML/ANE
    work + auto-downloads ungated models); this adapter runs it as a subprocess and parses
    its JSON speaker turns. Torch-free and ungated -- no HF token, no ~2 GB torch. FluidAudio's
    loader is file-based, so the samples are written to a temporary WAV for the tool to read.
    """

    def __init__(self, *, binary_path: Path) -> None:
        self._binary_path = binary_path

    def diarize(self, samples: Sequence[float], *, sample_rate: int) -> DiarizationResult:
        if not self._binary_path.exists():
            raise RuntimeError(
                f"hearsay-diarize not found at {self._binary_path}; build it with "
                "`swift build --package-path helper`"
            )
        pcm = (np.clip(np.asarray(samples, dtype=np.float32), -1.0, 1.0) * 32767.0).astype(np.int16)
        with tempfile.TemporaryDirectory() as tmp:
            wav_path = Path(tmp) / "them.wav"
            with wave.open(str(wav_path), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(sample_rate)
                wav.writeframes(pcm.tobytes())
            # Fixed argv (no shell); the tool prints JSON to stdout, diagnostics to stderr.
            proc = subprocess.run(
                [str(self._binary_path), str(wav_path)], capture_output=True, check=False
            )
        if proc.returncode != 0:
            tail = proc.stderr.decode("utf-8", "replace").strip().splitlines()
            raise RuntimeError(
                f"hearsay-diarize failed ({proc.returncode}): {tail[-1] if tail else ''}"
            )
        data = json.loads(proc.stdout)
        turns = [
            SpeakerTurn(
                speaker=str(turn["speaker"]),
                start_s=float(turn["start_s"]),
                end_s=float(turn["end_s"]),
            )
            for turn in data["turns"]
        ]
        embeddings = {
            str(entry["speaker"]): [float(value) for value in entry["embedding"]]
            for entry in data.get("speakers", [])
        }
        _log.info(
            "fluidaudio diarized: %d turns, %d speakers", len(turns), data.get("speaker_count", 0)
        )
        return DiarizationResult(turns=turns, embeddings=embeddings)


def diarize_helper_path(settings: Settings) -> Path:
    """The ``hearsay-diarize`` binary (a sibling of the capture helper in the same build dir)."""
    return settings.helper_path.with_name("hearsay-diarize")


def build_offline_diarizer(settings: Settings) -> OfflineDiarizer:
    """Construct the offline diarizer (FluidAudio pyannote community-1 CoreML on the ANE)."""
    return FluidAudioDiarizer(binary_path=diarize_helper_path(settings))
