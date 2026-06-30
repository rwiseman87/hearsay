"""Typed application settings (single source of configuration)."""

from __future__ import annotations

from pathlib import Path

from pydantic import AliasChoices, BaseModel, Field, SecretStr, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from hearsay.enums import (
    ASRBackendKind,
    DiarizationBackendKind,
    Environment,
    OfflineDiarizerKind,
)

# Local-first run-from-source layout: recordings, the SQLite DB, and models all live
# under the repo's outputs/ (gitignored). Packaging (Phase 5) can repoint these.
_OUTPUTS_DIR = Path(__file__).resolve().parents[3] / "outputs"


class ASRSettings(BaseModel):
    """ASR backend + model selection (swappable at runtime; see model store)."""

    backend: ASRBackendKind = ASRBackendKind.WHISPERCPP
    # A known model name (e.g. ``large-v3-turbo``) the backend resolves/downloads,
    # or an absolute path to a local model file.
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
    """Speaker diarization: torch-free ONNX embeddings + online clustering (Them only)."""

    enabled: bool = True
    backend: DiarizationBackendKind = DiarizationBackendKind.ONNX
    # A curated model name (see the embedding-model registry) or an absolute .onnx path.
    model: str = "wespeaker-cam++-lm"
    model_path: Path | None = None  # default: <models_dir>/<model filename>
    # Skip embedding utterances shorter than this: sub-second turns carry too little signal
    # for a reliable voiceprint, so they stay generic "Them" rather than risk mis-attribution.
    min_embed_ms: int = 1000
    # Cosine similarity at/above which an utterance joins an existing speaker vs starting a
    # new one. Clean-speech reference: same-speaker ~0.84, different ~0.2-0.33. Real-call
    # audio compresses that margin -- tune from the per-utterance cosines the diarizer logs.
    cluster_threshold: float = 0.5
    # Post-meeting re-diarization is the DEFAULT speaker path (2026-06-30): pyannote relabels the
    # Them track far better than the live online clusterer (on-device: 7 phantom speakers -> 2).
    # When on, the Them track is recorded to <folder>/them.wav so `hearsay rediarize` / the "Refine
    # speakers" button can run pyannote offline; needs the `diarization-pyannote` extra + HF login.
    # Privacy tradeoff: this retains raw audio by default -- set False to opt out (delete-meeting
    # also removes the folder).
    refine: bool = True
    # Which offline diarizer runs the refine. Default `fluidaudio` runs FluidAudio's
    # pyannote community-1 CoreML on the ANE via the `hearsay-diarize` helper (torch-free,
    # ungated -- no HF token). `pyannote` is the in-process torch path (needs the
    # `diarization-pyannote` extra + an HF login; the `pyannote_model`/`hf_token`/
    # `refine_device` fields below apply only to it).
    offline_backend: OfflineDiarizerKind = OfflineDiarizerKind.FLUIDAUDIO
    # pyannote pipeline + auth for the refine pass (used only when `offline_backend` is
    # `pyannote` and the `diarization-pyannote` extra is installed). hf_token defaults to None
    # -> use the huggingface CLI login; device "cpu" is safe on Apple Silicon ("mps" opt-in).
    pyannote_model: str = "pyannote/speaker-diarization-community-1"
    hf_token: SecretStr | None = None
    refine_device: str = "cpu"
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
