"""Offline diarizer seam -- FluidAudio (default) parses the helper JSON; pyannote loads lazily.

Real diarization is validated on-device (a recorded meeting), not here. FluidAudio's inference
lives in the Swift ``hearsay-diarize`` tool, so the unit suite mocks the subprocess and covers
the JSON parsing + failure handling; the opt-in pyannote path is just seam wiring (no torch
import / model load on construction).
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest
from pydantic import SecretStr

from hearsay.config.settings import Settings
from hearsay.diarization import (
    FluidAudioDiarizer,
    OfflineDiarizer,
    PyannoteDiarizer,
    SpeakerTurn,
    build_offline_diarizer,
    diarize_helper_path,
)
from hearsay.enums import OfflineDiarizerKind

_JSON = (
    b'{"sample_rate":16000,"duration_s":2.0,"speaker_count":2,'
    b'"turns":[{"speaker":"S1","start_s":0.0,"end_s":1.0},'
    b'{"speaker":"S2","start_s":1.0,"end_s":2.0}]}'
)


def test_default_offline_backend_is_fluidaudio() -> None:
    diarizer = build_offline_diarizer(Settings())
    assert isinstance(diarizer, FluidAudioDiarizer)
    assert isinstance(diarizer, OfflineDiarizer)  # runtime-checkable protocol


def test_diarize_helper_path_is_sibling_of_capture_helper() -> None:
    settings = Settings()
    expected = settings.helper_path.with_name("hearsay-diarize")
    assert diarize_helper_path(settings) == expected
    assert diarize_helper_path(settings).name == "hearsay-diarize"


def test_fluidaudio_diarizer_parses_helper_json(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    binary = tmp_path / "hearsay-diarize"
    binary.write_text("")  # the adapter checks the binary exists before invoking it

    def fake_run(args: list[str], **_: object) -> subprocess.CompletedProcess[bytes]:
        return subprocess.CompletedProcess(args, 0, stdout=_JSON, stderr=b"")

    monkeypatch.setattr(subprocess, "run", fake_run)
    turns = FluidAudioDiarizer(binary_path=binary).diarize([0.1, -0.1, 0.2], sample_rate=16000)
    assert turns == [SpeakerTurn("S1", 0.0, 1.0), SpeakerTurn("S2", 1.0, 2.0)]


def test_fluidaudio_diarizer_raises_on_helper_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    binary = tmp_path / "hearsay-diarize"
    binary.write_text("")

    def fake_run(args: list[str], **_: object) -> subprocess.CompletedProcess[bytes]:
        return subprocess.CompletedProcess(args, 1, stdout=b"", stderr=b'{"error":"boom"}\n')

    monkeypatch.setattr(subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="boom"):
        FluidAudioDiarizer(binary_path=binary).diarize([0.0], sample_rate=16000)


def test_fluidaudio_diarizer_raises_when_binary_missing(tmp_path: Path) -> None:
    with pytest.raises(RuntimeError, match="not found"):
        FluidAudioDiarizer(binary_path=tmp_path / "nope").diarize([0.0], sample_rate=16000)


def test_pyannote_backend_is_lazy_and_conforms() -> None:
    settings = Settings()
    settings.diarization.offline_backend = OfflineDiarizerKind.PYANNOTE
    diarizer = build_offline_diarizer(settings)
    assert isinstance(diarizer, PyannoteDiarizer)
    assert isinstance(diarizer, OfflineDiarizer)  # runtime-checkable protocol
    assert diarizer._pipeline is None  # no torch import / model load on construction


def test_pyannote_backend_carries_model_and_token() -> None:
    settings = Settings()
    settings.diarization.offline_backend = OfflineDiarizerKind.PYANNOTE
    settings.diarization.pyannote_model = "pyannote/custom-pipeline"
    settings.diarization.hf_token = SecretStr("hf_secret")
    diarizer = build_offline_diarizer(settings)
    assert isinstance(diarizer, PyannoteDiarizer)
    assert diarizer._model == "pyannote/custom-pipeline"
    assert diarizer._token == "hf_secret"  # SecretStr unwrapped for the HF API


def test_pyannote_backend_token_defaults_to_cli_login() -> None:
    settings = Settings()
    settings.diarization.offline_backend = OfflineDiarizerKind.PYANNOTE
    diarizer = build_offline_diarizer(settings)
    assert isinstance(diarizer, PyannoteDiarizer)
    assert diarizer._token is None  # None -> huggingface_hub falls back to the CLI login
