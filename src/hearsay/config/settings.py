"""Typed application settings (single source of configuration)."""

from __future__ import annotations

from pathlib import Path

from pydantic import AliasChoices, BaseModel, Field, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from hearsay.enums import ASRBackendKind, Environment


class ASRSettings(BaseModel):
    """ASR backend + model selection (swappable at runtime; see model store)."""

    backend: ASRBackendKind = ASRBackendKind.WHISPERCPP
    # A known model name (e.g. ``large-v3-turbo``) the backend resolves/downloads,
    # or an absolute path to a local model file.
    model: str = "large-v3-turbo"
    language: str | None = None  # None = auto-detect


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
    app_support_dir: Path = Field(
        default_factory=lambda: Path.home() / "Library" / "Application Support" / "hearsay"
    )
    output_dir: Path = Field(default_factory=lambda: Path.home() / "Documents" / "hearsay")
    models_dir: Path | None = None  # default: <app_support_dir>/models
    asr: ASRSettings = Field(default_factory=ASRSettings)
    vad: VADSettings = Field(default_factory=VADSettings)
    helper_path: Path = Field(
        default_factory=lambda: (
            Path(__file__).resolve().parents[3] / "helper" / ".build" / "debug" / "hearsay-helper"
        )
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
            self.database_url = f"sqlite+aiosqlite:///{self.app_support_dir / 'hearsay.db'}"
        if self.models_dir is None:
            self.models_dir = self.app_support_dir / "models"
        if self.vad.model_path is None:
            self.vad.model_path = self.models_dir / "silero_vad.onnx"
        return self
