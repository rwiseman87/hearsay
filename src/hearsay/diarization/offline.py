"""Offline (post-meeting) diarization: whole-track speaker-turn segmentation.

Unlike the online :class:`~hearsay.fusion.OnlineSpeakerClusterer` (one embedding per
VAD utterance), an offline diarizer runs over the *entire* Them track at once, so it can
do sliding-window segmentation + global clustering + overlap handling -- which is what
the online path cannot do when the VAD hands it multi-speaker utterances. The default
backend wraps pyannote (the accuracy-max opt-in); the seam keeps it swappable and lets
the refine orchestration be unit-tested with a stub.
"""

from __future__ import annotations

import json
import subprocess
import tempfile
import warnings
import wave
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

from hearsay.enums import OfflineDiarizerKind
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
    def diarize(self, samples: Sequence[float], *, sample_rate: int) -> list[SpeakerTurn]:
        """Return speaker turns over ``samples`` (mono float in [-1, 1])."""
        ...


class PyannoteDiarizer:
    """pyannote ``speaker-diarization-community-1`` over an in-memory waveform.

    The pipeline (heavy: torch) loads lazily on first :meth:`diarize`. pyannote is fed a
    torch tensor rather than a file path, so it never touches torchcodec/ffmpeg decoding.
    """

    def __init__(self, *, model: str, token: str | None = None, device: str = "cpu") -> None:
        self._model = model
        self._token = token
        self._device = device
        self._pipeline: Any | None = None

    def _ensure_pipeline(self) -> Any:
        if self._pipeline is None:
            import logging  # noqa: PLC0415

            import torch  # noqa: PLC0415 (optional dep; only when refining)

            # huggingface_hub logs every model-file HEAD over httpx at INFO; quiet it.
            logging.getLogger("httpx").setLevel(logging.WARNING)

            # torchcodec fails to load without ffmpeg and warns loudly (a multi-line traceback)
            # at import; we feed an in-memory tensor (never decode a file), so it's pure noise.
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                from pyannote.audio import Pipeline  # noqa: PLC0415

            pipeline = Pipeline.from_pretrained(self._model, token=self._token)
            if pipeline is None:
                raise RuntimeError(
                    f"could not load pyannote pipeline '{self._model}'; check the "
                    "diarization-pyannote extra, the HF login, and gated-model access"
                )
            self._pipeline = pipeline.to(torch.device(self._device))
        return self._pipeline

    def diarize(self, samples: Sequence[float], *, sample_rate: int) -> list[SpeakerTurn]:
        import torch  # noqa: PLC0415

        pipeline = self._ensure_pipeline()
        waveform = torch.tensor(samples, dtype=torch.float32).unsqueeze(0)  # (1, num_samples)
        with warnings.catch_warnings():
            # pyannote's pooling warns "std(): degrees of freedom <= 0" on very short frames.
            warnings.filterwarnings("ignore", message=".*degrees of freedom.*")
            output = pipeline({"waveform": waveform, "sample_rate": sample_rate})
        # Exclusive diarization assigns each instant to at most one speaker, which maps
        # cleanly onto transcript segments (overlap regions are split, not double-labelled).
        annotation = output.exclusive_speaker_diarization
        turns = [
            SpeakerTurn(speaker=str(label), start_s=float(segment.start), end_s=float(segment.end))
            for segment, _, label in annotation.itertracks(yield_label=True)
        ]
        _log.info(
            "pyannote diarized: %d turns, %d speakers", len(turns), len({t.speaker for t in turns})
        )
        return turns


class FluidAudioDiarizer:
    """FluidAudio's pyannote community-1 CoreML diarizer, run on the ANE via the helper.

    Offline inference lives in the Swift ``hearsay-diarize`` tool (it owns the CoreML/ANE
    work + auto-downloads ungated models); this adapter runs it as a subprocess and parses
    its JSON speaker turns. Torch-free and ungated -- no HF token, no ~2 GB torch. FluidAudio's
    loader is file-based, so the samples are written to a temporary WAV for the tool to read.
    """

    def __init__(self, *, binary_path: Path) -> None:
        self._binary_path = binary_path

    def diarize(self, samples: Sequence[float], *, sample_rate: int) -> list[SpeakerTurn]:
        import numpy as np  # noqa: PLC0415 (optional dep; only present when refining)

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
        _log.info(
            "fluidaudio diarized: %d turns, %d speakers", len(turns), data.get("speaker_count", 0)
        )
        return turns


def diarize_helper_path(settings: Settings) -> Path:
    """The ``hearsay-diarize`` binary (a sibling of the capture helper in the same build dir)."""
    return settings.helper_path.with_name("hearsay-diarize")


def build_offline_diarizer(settings: Settings) -> OfflineDiarizer:
    """Construct the configured offline diarizer (FluidAudio default; pyannote opt-in)."""
    diarization = settings.diarization
    if diarization.offline_backend is OfflineDiarizerKind.FLUIDAUDIO:
        return FluidAudioDiarizer(binary_path=diarize_helper_path(settings))
    token = diarization.hf_token.get_secret_value() if diarization.hf_token is not None else None
    return PyannoteDiarizer(
        model=diarization.pyannote_model, token=token, device=diarization.refine_device
    )
