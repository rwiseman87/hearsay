"""ASR backends behind a swappable seam."""

from __future__ import annotations

from hearsay.asr.base import SAMPLE_RATE, ASRBackend, ASRSegment
from hearsay.asr.manager import (
    KNOWN_MODELS,
    KnownModel,
    ModelInfo,
    asr_helper_path,
    available_models,
    build_asr,
    resolve_model,
)
from hearsay.asr.parakeet_backend import ParakeetBackend

__all__ = [
    "KNOWN_MODELS",
    "SAMPLE_RATE",
    "ASRBackend",
    "ASRSegment",
    "KnownModel",
    "ModelInfo",
    "ParakeetBackend",
    "asr_helper_path",
    "available_models",
    "build_asr",
    "resolve_model",
]
