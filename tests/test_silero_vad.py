"""Silero VAD integration test.

Skipped unless onnxruntime is installed (the `asr`/`diarization` extra). When it is,
this downloads the pinned model and runs real inference, so it doubles as the
on-device smoke test for the VAD path.
"""

from __future__ import annotations

import importlib.util
import struct
import urllib.request
import wave
from pathlib import Path

import pytest

from hearsay.vad.silero import FRAME_SAMPLES, SileroVAD, download_silero_model

pytestmark = pytest.mark.skipif(
    importlib.util.find_spec("onnxruntime") is None, reason="onnxruntime not installed"
)

_JFK_URL = "https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav"


def _frames(samples: list[float]) -> list[list[float]]:
    return [
        samples[i : i + FRAME_SAMPLES]
        for i in range(0, len(samples) - FRAME_SAMPLES, FRAME_SAMPLES)
    ]


def test_silero_scores_silence_near_zero(tmp_path: Path) -> None:
    vad = SileroVAD(download_silero_model(tmp_path / "silero_vad.onnx"))
    assert vad.frame_samples == FRAME_SAMPLES
    probs = [vad.speech_prob(frame) for frame in _frames([0.0] * (FRAME_SAMPLES * 20))]
    assert max(probs) < 0.5


def test_silero_detects_real_speech(tmp_path: Path) -> None:
    # Guards the left-context regression: without the 64-sample context every frame
    # scores ~0 even on clear speech.
    wav = tmp_path / "jfk.wav"
    urllib.request.urlretrieve(_JFK_URL, wav)
    with wave.open(str(wav)) as handle:
        raw = handle.readframes(handle.getnframes())
    samples = [v / 32768.0 for v in struct.unpack(f"<{len(raw) // 2}h", raw)]

    vad = SileroVAD(download_silero_model(tmp_path / "silero_vad.onnx"))
    probs = [vad.speech_prob(frame) for frame in _frames(samples)]
    speech_frames = sum(1 for prob in probs if prob >= 0.5)
    assert speech_frames > 50  # JFK is ~11s of continuous speech
    assert max(probs) > 0.9
