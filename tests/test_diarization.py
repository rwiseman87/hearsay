"""Speaker-embedding tests.

The registry/path/download-logic tests need no heavy deps and run everywhere. The
fbank + real-model tests need the ``diarization`` extra (onnxruntime,
kaldi-native-fbank) and skip without it; when present they download the pinned
wespeaker model + real speech and assert speaker discrimination, doubling as the
on-device smoke test for the diarization path.
"""

from __future__ import annotations

import importlib.util
import struct
import urllib.request
import wave
from pathlib import Path

import pytest

from hearsay.config.settings import Settings
from hearsay.diarization import (
    KNOWN_EMBEDDING_MODELS,
    build_embedder,
    download_embedding_model,
    embedding_model_path,
    resolve_embedding_model,
)
from hearsay.diarization import manager as diar_manager

_HAS_DEPS = (
    importlib.util.find_spec("onnxruntime") is not None
    and importlib.util.find_spec("kaldi_native_fbank") is not None
)
needs_deps = pytest.mark.skipif(not _HAS_DEPS, reason="diarization extra not installed")

_JFK_URL = "https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav"
_SPEAKER_B_URL = (
    "https://raw.githubusercontent.com/jameslyons/python_speech_features/master/english.wav"
)


def _load_wav(url: str, dst: Path) -> list[float]:
    if not dst.exists():
        urllib.request.urlretrieve(url, dst)
    with wave.open(str(dst)) as handle:
        raw = handle.readframes(handle.getnframes())
    return [v / 32768.0 for v in struct.unpack(f"<{len(raw) // 2}h", raw)]


# --- registry + manager (no heavy deps) ---


def test_registry_resolves_default_model() -> None:
    model = resolve_embedding_model("wespeaker-cam++-lm")
    assert model is not None
    assert model.dim == 512
    assert model.license == "CC-BY-4.0"
    assert model.url.startswith("https://") and model.sha256
    assert resolve_embedding_model("does-not-exist") is None


def test_embedding_model_path_default_and_override(tmp_path: Path) -> None:
    settings = Settings(models_dir=tmp_path)
    default = embedding_model_path(settings)
    assert default.parent == tmp_path
    assert default.name == KNOWN_EMBEDDING_MODELS[0].filename

    settings.diarization.model_path = tmp_path / "custom.onnx"
    assert embedding_model_path(settings) == tmp_path / "custom.onnx"


def test_download_embedding_model_rejects_bad_checksum(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    class _FakeResponse:
        def __enter__(self) -> _FakeResponse:
            return self

        def __exit__(self, *_: object) -> bool:
            return False

        def read(self) -> bytes:
            return b"corrupted bytes, not the model"

    monkeypatch.setattr(diar_manager.urllib.request, "urlopen", lambda *a, **k: _FakeResponse())
    with pytest.raises(ValueError, match="checksum mismatch"):
        download_embedding_model(KNOWN_EMBEDDING_MODELS[0], tmp_path)
    assert not (tmp_path / KNOWN_EMBEDDING_MODELS[0].filename).exists()


# --- real model (needs the diarization extra) ---


@pytest.fixture(scope="session")
def models_dir(tmp_path_factory: pytest.TempPathFactory) -> Path:
    path = tmp_path_factory.mktemp("emb")
    download_embedding_model(KNOWN_EMBEDDING_MODELS[0], path)
    return path


@needs_deps
def test_embedder_unit_norm_and_deterministic(models_dir: Path) -> None:
    import numpy as np  # noqa: PLC0415 (optional dep; lazy so the file imports without it)

    embedder = build_embedder(Settings(models_dir=models_dir))
    assert embedder.dim == 512
    assert embedder.model_id == "wespeaker-cam++-lm"

    sig = [float(np.sin(2 * np.pi * 200 * i / 16000) * 0.2) for i in range(16000)]
    e1 = embedder.embed(sig)
    e2 = embedder.embed(sig)
    assert e1.shape == (512,)
    assert abs(float(np.linalg.norm(e1)) - 1.0) < 1e-4
    assert float(np.dot(e1, e2)) > 0.999  # deterministic


@needs_deps
def test_embedder_discriminates_speakers(models_dir: Path, tmp_path: Path) -> None:
    import numpy as np  # noqa: PLC0415 (optional dep; lazy so the file imports without it)

    embedder = build_embedder(Settings(models_dir=models_dir))
    jfk = _load_wav(_JFK_URL, tmp_path / "jfk.wav")
    other = _load_wav(_SPEAKER_B_URL, tmp_path / "english.wav")

    half = len(jfk) // 2
    same = float(np.dot(embedder.embed(jfk[:half]), embedder.embed(jfk[half:])))
    diff = float(np.dot(embedder.embed(jfk[:half]), embedder.embed(other)))
    assert same > 0.6  # same speaker (JFK halves) -- observed ~0.84
    assert same - diff > 0.2  # clearly separated from a different speaker (observed diff ~0.33)
