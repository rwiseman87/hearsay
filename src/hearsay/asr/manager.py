"""ASR backend selection + model resolution (the swap-at-whim entry point).

``build_asr`` instantiates the configured backend; ``resolve_model`` maps a friendly
name (``large-v3-turbo``) to the identifier a backend understands (a whisper.cpp GGML
name vs an MLX Hugging Face repo). Unknown names pass through, so an absolute path or
an explicit repo also works. Heavy backend deps load only when a backend is built.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from hearsay.asr.base import ASRBackend
from hearsay.asr.mlx_backend import MlxBackend
from hearsay.asr.parakeet_backend import ParakeetBackend
from hearsay.asr.whispercpp_backend import WhisperCppBackend
from hearsay.config.settings import Settings
from hearsay.enums import ASRBackendKind


@dataclass(frozen=True, slots=True)
class KnownModel:
    name: str
    label: str
    whispercpp: str  # GGML name pywhispercpp downloads
    mlx: str  # MLX-format Hugging Face repo (verified to exist)


# Curated, verified models for the picker; any other name/path/repo passes through.
KNOWN_MODELS: tuple[KnownModel, ...] = (
    KnownModel(
        "large-v3-turbo", "Large v3 Turbo", "large-v3-turbo", "mlx-community/whisper-large-v3-turbo"
    ),
    KnownModel("large-v3", "Large v3", "large-v3", "mlx-community/whisper-large-v3-mlx"),
    KnownModel("base", "Base (fast)", "base", "mlx-community/whisper-base-mlx"),
)
_BY_NAME = {model.name: model for model in KNOWN_MODELS}


@dataclass(frozen=True, slots=True)
class ModelInfo:
    name: str
    label: str
    installed: bool


def resolve_model(kind: ASRBackendKind, model: str) -> str:
    known = _BY_NAME.get(model)
    if known is None:
        return model  # path, or a backend-specific id the user supplied directly
    return known.whispercpp if kind is ASRBackendKind.WHISPERCPP else known.mlx


def asr_helper_path(settings: Settings) -> Path:
    """The ``hearsay-asr`` sidecar binary (a sibling of the capture helper in the build dir)."""
    return settings.helper_path.with_name("hearsay-asr")


def build_asr(settings: Settings) -> ASRBackend:
    kind = settings.asr.backend
    if kind is ASRBackendKind.PARAKEET:
        # Parakeet uses its own bundled model; the whisper-centric `model` field doesn't apply.
        return ParakeetBackend(binary_path=asr_helper_path(settings))
    model = resolve_model(kind, settings.asr.model)
    if kind is ASRBackendKind.WHISPERCPP:
        return WhisperCppBackend(
            model, models_dir=settings.models_dir, beam_size=settings.asr.beam_size
        )
    if kind is ASRBackendKind.MLX:
        return MlxBackend(model)
    raise ValueError(f"unknown ASR backend: {kind}")


def available_models(settings: Settings) -> list[ModelInfo]:
    """Curated models plus any local GGML files already in the models dir."""
    infos = [
        ModelInfo(name=model.name, label=model.label, installed=False) for model in KNOWN_MODELS
    ]
    models_dir = settings.models_dir
    if models_dir is not None and models_dir.is_dir():
        for path in sorted(models_dir.glob("*.bin")):
            infos.append(ModelInfo(name=str(path), label=path.name, installed=True))
    return infos
