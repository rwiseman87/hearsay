"""ASR backends behind a swappable seam."""

from __future__ import annotations

from hearsay.asr.base import SAMPLE_RATE, ASRBackend, ASRSegment
from hearsay.asr.manager import (
    ModelInfo,
    asr_helper_path,
    available_models,
    build_asr,
)
from hearsay.asr.parakeet_backend import ParakeetBackend

__all__ = [
    "SAMPLE_RATE",
    "ASRBackend",
    "ASRSegment",
    "ModelInfo",
    "ParakeetBackend",
    "asr_helper_path",
    "available_models",
    "build_asr",
]
