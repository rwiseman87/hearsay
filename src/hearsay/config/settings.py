"""Typed application settings (single source of configuration)."""

from __future__ import annotations

from pathlib import Path

from pydantic import AliasChoices, BaseModel, Field, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from hearsay.enums import ASRBackendKind, DiarizationBackendKind, Environment

# Local-first run-from-source layout: recordings, the SQLite DB, and models all live
# under the repo's outputs/ (gitignored). Packaging (Phase 5) can repoint these.
_OUTPUTS_DIR = Path(__file__).resolve().parents[3] / "outputs"


class ASRSettings(BaseModel):
    """ASR backend + model selection (swappable at runtime; see model store)."""

    backend: ASRBackendKind = ASRBackendKind.WHISPERCPP
    # A known model name (e.g. ``large-v3-turbo``) the backend resolves/downloads,
    # or an absolute path to a local model file.
    model: str = "large-v3-turbo"
    language: str | None = None  # None = auto-detect


class DiarizationSettings(BaseModel):
    """Speaker diarization: torch-free ONNX embeddings + online clustering (Them only)."""

    enabled: bool = True
    backend: DiarizationBackendKind = DiarizationBackendKind.ONNX
    # A curated model name (see the embedding-model registry) or an absolute .onnx path.
    model: str = "wespeaker-cam++-lm"
    model_path: Path | None = None  # default: <models_dir>/<model filename>
    # Skip embedding utterances shorter than this (too little signal -> noisy voiceprint).
    min_embed_ms: int = 500
    # Cosine similarity at/above which an utterance joins an existing speaker vs starting
    # a new one (validated: same-speaker ~0.84, different ~0.2-0.33, so ~0.5 separates).
    cluster_threshold: float = 0.5


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
