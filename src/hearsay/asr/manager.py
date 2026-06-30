"""ASR backend construction. Live ASR is FluidAudio Parakeet TDT on the ANE, via the
Swift ``hearsay-asr`` sidecar (Parakeet has a single bundled model, so there is no picker)."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from hearsay.asr.base import ASRBackend
from hearsay.asr.parakeet_backend import ParakeetBackend
from hearsay.config.settings import Settings


@dataclass(frozen=True, slots=True)
class ModelInfo:
    name: str
    label: str
    installed: bool


def asr_helper_path(settings: Settings) -> Path:
    """The ``hearsay-asr`` sidecar binary (a sibling of the capture helper in the build dir)."""
    return settings.helper_path.with_name("hearsay-asr")


def build_asr(settings: Settings) -> ASRBackend:
    """Construct the ASR backend (Parakeet TDT on the ANE)."""
    return ParakeetBackend(binary_path=asr_helper_path(settings))


def available_models(settings: Settings) -> list[ModelInfo]:
    """The active ASR model (Parakeet ships a single model, run on the ANE)."""
    return [ModelInfo(name="parakeet-tdt-v3", label="Parakeet TDT v3 (ANE)", installed=True)]
