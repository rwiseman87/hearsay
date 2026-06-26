"""Speaker-embedding model registry + backend selection.

Curated, license-vetted ONNX embedding models, downloaded ungated and pinned by sha256
(same supply-chain posture as the Silero VAD). ``build_embedder`` instantiates the
configured backend; heavy deps (onnxruntime, kaldi-native-fbank) load only when built.
"""

from __future__ import annotations

import hashlib
import urllib.request
from dataclasses import dataclass
from pathlib import Path

from hearsay.config.settings import Settings
from hearsay.diarization.base import SpeakerEmbedder
from hearsay.diarization.onnx_embedder import OnnxSpeakerEmbedder
from hearsay.enums import DiarizationBackendKind
from hearsay.log import get_logger

_log = get_logger("hearsay.diarization")


@dataclass(frozen=True, slots=True)
class EmbeddingModel:
    name: str
    filename: str
    url: str
    sha256: str
    dim: int
    license: str


# Hosted ungated by the Next-gen Kaldi (k2-fsa) project; weights are CC-BY-4.0
# (redistributable with attribution, so bundle-able into a distributed app).
KNOWN_EMBEDDING_MODELS: tuple[EmbeddingModel, ...] = (
    EmbeddingModel(
        name="wespeaker-cam++-lm",
        filename="wespeaker_en_voxceleb_CAM++_LM.onnx",
        url=(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/"
            "speaker-recongition-models/wespeaker_en_voxceleb_CAM++_LM.onnx"
        ),
        sha256="e197af7e9d473030cf486b3124149a19bf37014d0e4485e4c70c483b0ec10cb2",
        dim=512,
        license="CC-BY-4.0",
    ),
)
_BY_NAME = {model.name: model for model in KNOWN_EMBEDDING_MODELS}


def resolve_embedding_model(name: str) -> EmbeddingModel | None:
    """Look up a curated model by name (``None`` for an unknown name / explicit path)."""
    return _BY_NAME.get(name)


def embedding_model_path(settings: Settings) -> Path:
    """Resolve the on-disk model path (explicit override, else ``<models_dir>/<file>``)."""
    diarization = settings.diarization
    if diarization.model_path is not None:
        return diarization.model_path
    model = _BY_NAME.get(diarization.model)
    filename = model.filename if model is not None else f"{diarization.model}.onnx"
    assert settings.models_dir is not None  # filled by Settings' validator
    return settings.models_dir / filename


def download_embedding_model(model: EmbeddingModel, dest_dir: Path) -> Path:
    """Download + verify the model to ``dest_dir`` (idempotent; sha256-checked)."""
    dest = dest_dir / model.filename
    if dest.exists() and hashlib.sha256(dest.read_bytes()).hexdigest() == model.sha256:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    _log.info("downloading speaker-embedding model %s to %s", model.name, dest)
    with urllib.request.urlopen(model.url, timeout=120) as response:
        data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != model.sha256:
        raise ValueError(f"{model.name} checksum mismatch: got {digest}, expected {model.sha256}")
    dest.write_bytes(data)
    return dest


def build_embedder(settings: Settings) -> SpeakerEmbedder:
    """Instantiate the configured speaker-embedding backend."""
    kind = settings.diarization.backend
    if kind is DiarizationBackendKind.ONNX:
        path = embedding_model_path(settings)
        model = _BY_NAME.get(settings.diarization.model)
        return OnnxSpeakerEmbedder(path, model_id=model.name if model is not None else path.stem)
    raise ValueError(f"unknown diarization backend: {kind}")
