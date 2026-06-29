"""Offline diarizer seam -- construction is torch-free; pyannote loads lazily.

The real pyannote diarization is validated on-device (a recorded meeting), not here:
loading the gated model pulls torch + network, so the unit suite only covers the seam
wiring. The refine *mapping* logic is unit-tested separately with a stub diarizer.
"""

from __future__ import annotations

from pydantic import SecretStr

from hearsay.config.settings import Settings
from hearsay.diarization import OfflineDiarizer, PyannoteDiarizer, build_offline_diarizer


def test_build_offline_diarizer_is_lazy_and_conforms() -> None:
    diarizer = build_offline_diarizer(Settings())
    assert isinstance(diarizer, PyannoteDiarizer)
    assert isinstance(diarizer, OfflineDiarizer)  # runtime-checkable protocol
    assert diarizer._pipeline is None  # no torch import / model load on construction


def test_offline_diarizer_carries_model_and_token() -> None:
    settings = Settings()
    settings.diarization.pyannote_model = "pyannote/custom-pipeline"
    settings.diarization.hf_token = SecretStr("hf_secret")
    diarizer = build_offline_diarizer(settings)
    assert isinstance(diarizer, PyannoteDiarizer)
    assert diarizer._model == "pyannote/custom-pipeline"
    assert diarizer._token == "hf_secret"  # SecretStr unwrapped for the HF API


def test_offline_diarizer_token_defaults_to_cli_login() -> None:
    # No explicit token -> None, so huggingface_hub falls back to the `hf auth login` token.
    diarizer = build_offline_diarizer(Settings())
    assert isinstance(diarizer, PyannoteDiarizer)
    assert diarizer._token is None
