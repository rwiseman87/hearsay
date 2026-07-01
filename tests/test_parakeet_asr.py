"""Parakeet ASR backend: stdio framing + lifecycle against a fake sidecar.

The real transcription runs in the Swift ``hearsay-asr`` process (validated on-device); here
we mock ``subprocess.Popen`` so the request framing, response parsing, error handling, and
teardown are covered in CI without the binary or a model.
"""

from __future__ import annotations

import io
import struct
import subprocess
from pathlib import Path

import numpy as np
import pytest

from hearsay.asr import ASRBackend, ParakeetBackend, asr_helper_path, build_asr
from hearsay.config.settings import Settings


class _FakeProc:
    def __init__(self, response: bytes) -> None:
        self.stdin = io.BytesIO()
        self.stdout = io.BytesIO(response)
        self.stderr = None
        self._alive = True
        self.waited = False

    def poll(self) -> int | None:
        return None if self._alive else 0

    def wait(self, timeout: float | None = None) -> int:
        self.waited = True
        self._alive = False
        return 0

    def kill(self) -> None:
        self._alive = False


def _backend(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, response: bytes) -> ParakeetBackend:
    binary = tmp_path / "hearsay-asr"
    binary.write_text("")  # the backend checks the binary exists before spawning
    fake = _FakeProc(response)
    monkeypatch.setattr(subprocess, "Popen", lambda *_a, **_k: fake)
    backend = ParakeetBackend(binary_path=binary)
    backend._fake = fake  # type: ignore[attr-defined]  # expose for assertions
    return backend


def test_build_asr_defaults_to_parakeet() -> None:
    backend = build_asr(Settings())
    assert isinstance(backend, ParakeetBackend)
    assert isinstance(backend, ASRBackend)  # runtime-checkable protocol (incl. close)
    assert backend.model == "parakeet-tdt-v3"


def test_asr_helper_path_is_sibling_of_capture_helper() -> None:
    settings = Settings()
    assert asr_helper_path(settings) == settings.helper_path.with_name("hearsay-asr")


def test_transcribe_frames_request_and_parses_response(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    backend = _backend(tmp_path, monkeypatch, b'{"text":"hello there"}\n')
    segments = backend.transcribe([0.5, -0.5, 0.25])

    assert [s.text for s in segments] == ["hello there"]
    assert segments[0].start_s == 0.0
    assert segments[0].end_s == pytest.approx(3 / 16_000)
    # The request is <uint32 LE count><count float32 LE samples>.
    sent = backend._fake.stdin.getvalue()  # type: ignore[attr-defined]
    assert struct.unpack("<I", sent[:4])[0] == 3
    assert np.frombuffer(sent[4:], dtype="<f4").tolist() == pytest.approx([0.5, -0.5, 0.25])


def test_empty_text_yields_no_segments(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    backend = _backend(tmp_path, monkeypatch, b'{"text":""}\n')
    assert backend.transcribe([0.1, 0.2]) == []


def test_missing_binary_raises(tmp_path: Path) -> None:
    backend = ParakeetBackend(binary_path=tmp_path / "nope")
    with pytest.raises(RuntimeError, match="not found"):
        backend.transcribe([0.0])


def test_eof_from_sidecar_raises(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    backend = _backend(tmp_path, monkeypatch, b"")  # sidecar died -> empty read
    with pytest.raises(RuntimeError, match="no output"):
        backend.transcribe([0.0, 0.1])


def test_close_terminates_sidecar(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    backend = _backend(tmp_path, monkeypatch, b'{"text":"x"}\n')
    backend.transcribe([0.0])  # spawn it
    backend.close()
    assert backend._fake.waited  # type: ignore[attr-defined]
    backend.close()  # idempotent
