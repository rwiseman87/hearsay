"""Typed application settings (single source of configuration)."""

from __future__ import annotations

from pathlib import Path

from pydantic import AliasChoices, BaseModel, Field, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from hearsay.enums import ASRBackendKind, Environment

# Local-first run-from-source layout: recordings, the SQLite DB, and models all live
# under the repo's outputs/ (gitignored). Packaging (Phase 5) can repoint these.
_OUTPUTS_DIR = Path(__file__).resolve().parents[3] / "outputs"


class ASRSettings(BaseModel):
    """ASR backend + model selection (swappable at runtime; see model store)."""

    # Default Parakeet (FluidAudio on the ANE) -- no whisper.cpp/Metal, which could enter an
    # unrecoverable error state and silently stop transcribing. whisper.cpp/mlx stay available.
    backend: ASRBackendKind = ASRBackendKind.PARAKEET
    # A known model name (e.g. ``large-v3-turbo``) the backend resolves/downloads, or an
    # absolute path. Applies to whisper.cpp/mlx; Parakeet uses its own bundled model.
    model: str = "large-v3"
    language: str | None = None  # None = auto-detect
    # Optional decoding knobs (whisper.cpp), off by default: an on-device A/B showed the model
    # choice -- not these -- drove accuracy, and beam search slows live transcription. Kept
    # config-gated for future tuning. beam_size > 1 enables beam search (1 = greedy).
    beam_size: int = 1
    # When on, feed the previous final's text as a decoding prompt so names/terms stay consistent
    # across utterances; reset after a silence gap so a wrong prompt cannot snowball.
    condition_on_previous_text: bool = False
    context_reset_gap_s: float = 8.0


class DiarizationSettings(BaseModel):
    """Speaker diarization: post-meeting refine + cross-meeting voiceprint recognition.

    All inference runs in Swift on the ANE: live "Them" labels come from the hearsay-live
    sidecar, and the post-meeting refine (the hearsay-diarize helper) re-labels the whole
    Them track + emits per-speaker voiceprints. Nothing here selects a model.
    """

    # Post-meeting re-diarization is the DEFAULT speaker path: FluidAudio (on the ANE) relabels
    # the whole Them track far better than the live sidecar's streaming labels. When on, the Them
    # track is recorded to <folder>/them.wav so `hearsay rediarize` / the "Refine speakers" button
    # can run the offline diarizer. Privacy tradeoff: this retains raw audio by default -- set
    # False to opt out (delete-meeting also removes the folder).
    refine: bool = True
    # Run the offline refine automatically when a meeting finalizes (vs. only on the manual
    # "Refine speakers" button / `hearsay rediarize`). Cheap now that the diarizer is FluidAudio
    # on the ANE (~seconds), so every meeting ends with accurate labels. Gated on `refine`
    # (needs them.wav); a missing recording or a diarizer error never blocks the stop.
    auto_refine: bool = True
    # Cosine at/above which a refined speaker's voiceprint is auto-matched to a person named
    # in a previous meeting. Conservative (a wrong cross-meeting match is worse than none).
    recognition_threshold: float = 0.6


class VADSettings(BaseModel):
    """Silero VAD thresholds + segmentation hysteresis."""

    threshold: float = 0.5
    min_speech_ms: int = 250
    min_silence_ms: int = 600
    # Cadence for live partial transcripts during ongoing speech (0 disables partials).
    partial_ms: int = 800
    model_path: Path | None = None  # default: <models_dir>/silero_vad.onnx


class Settings(BaseSettings):
    """Application settings.

    Most fields read ``HEARSAY_``-prefixed environment variables. ``environment``
    and ``database_url`` also accept the bare ``ENVIRONMENT`` / ``DATABASE_URL``
    names documented in CLAUDE.md.
    """

    model_config = SettingsConfigDict(
        env_prefix="HEARSAY_",
        env_nested_delimiter="__",
        extra="ignore",
    )

    environment: Environment = Field(
        default=Environment.DEVELOPMENT,
        validation_alias=AliasChoices("HEARSAY_ENVIRONMENT", "ENVIRONMENT"),
    )
    output_dir: Path = Field(default_factory=lambda: _OUTPUTS_DIR / "recordings")
    capture_debug_dir: Path = Field(default_factory=lambda: _OUTPUTS_DIR / "capture-debug")
    models_dir: Path | None = None  # default: <outputs>/models
    asr: ASRSettings = Field(default_factory=ASRSettings)
    vad: VADSettings = Field(default_factory=VADSettings)
    diarization: DiarizationSettings = Field(default_factory=DiarizationSettings)
    helper_path: Path = Field(
        default_factory=lambda: (
            Path(__file__).resolve().parents[3] / "helper" / ".build" / "debug" / "hearsay-helper"
        )
    )
    web_dir: Path = Field(
        default_factory=lambda: Path(__file__).resolve().parents[3] / "web" / "dist"
    )
    database_url: str | None = Field(
        default=None,
        validation_alias=AliasChoices("HEARSAY_DATABASE_URL", "DATABASE_URL"),
    )
    server_host: str = "127.0.0.1"
    server_port: int = 0  # 0 -> auto-pick a free port

    @model_validator(mode="after")
    def _fill_derived_paths(self) -> Settings:
        if self.database_url is None:
            self.database_url = f"sqlite+aiosqlite:///{_OUTPUTS_DIR / 'db' / 'hearsay.db'}"
        if self.models_dir is None:
            self.models_dir = _OUTPUTS_DIR / "models"
        if self.vad.model_path is None:
            self.vad.model_path = self.models_dir / "silero_vad.onnx"
        return self
