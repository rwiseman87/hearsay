"""ASR backend construction. The post-meeting refine's ASR is FluidAudio Parakeet TDT on the
ANE, via the Swift ``hearsay-asr`` sidecar (a single bundled model)."""

from __future__ import annotations

from pathlib import Path

from hearsay.asr.base import ASRBackend
from hearsay.asr.parakeet_backend import ParakeetBackend
from hearsay.config.settings import Settings


def asr_helper_path(settings: Settings) -> Path:
    """The ``hearsay-asr`` sidecar binary (a sibling of the capture helper in the build dir)."""
    return settings.helper_path.with_name("hearsay-asr")


def build_asr(settings: Settings) -> ASRBackend:
    """Construct the ASR backend (Parakeet TDT on the ANE)."""
    return ParakeetBackend(binary_path=asr_helper_path(settings))
