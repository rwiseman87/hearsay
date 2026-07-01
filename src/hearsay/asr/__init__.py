"""ASR backend for the post-meeting refine (FluidAudio Parakeet on the ANE)."""

from __future__ import annotations

from hearsay.asr.base import SAMPLE_RATE, ASRBackend, ASRSegment
from hearsay.asr.manager import asr_helper_path, build_asr
from hearsay.asr.parakeet_backend import ParakeetBackend

__all__ = [
    "SAMPLE_RATE",
    "ASRBackend",
    "ASRSegment",
    "ParakeetBackend",
    "asr_helper_path",
    "build_asr",
]
