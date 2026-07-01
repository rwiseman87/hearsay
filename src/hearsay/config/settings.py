"""Typed application settings (single source of configuration)."""

from __future__ import annotations

from pathlib import Path

from pydantic import AliasChoices, BaseModel, Field, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

from hearsay.enums import Environment

# Local-first run-from-source layout: recordings, the SQLite DB, and models all live
# under the repo's outputs/ (gitignored). Packaging (Phase 5) can repoint these.
_OUTPUTS_DIR = Path(__file__).resolve().parents[3] / "outputs"


class DiarizationSettings(BaseModel):
    """Speaker diarization: post-meeting refine + cross-meeting voiceprint recognition.

    All inference runs in Swift on the ANE: live "Them" labels come from the hearsay-live
    sidecar, and the post-meeting refine (the hearsay-diarize helper) re-labels the whole
    Them track + emits per-speaker voiceprints. Nothing here selects a model.
    """

    # Post-meeting re-diarization is the DEFAULT speaker path: FluidAudio (on the ANE) relabels
    # the whole Them track far better than the live sidecar's streaming labels, reading the Them
    # channel of the recorded <folder>/audio.wav. So it needs `audio.record` on (the master
    # audio-retention switch); with it off there is no recording to re-diarize and the refine skips.
    refine: bool = True
    # Run the offline refine automatically when a meeting finalizes (vs. only on the manual
    # "Refine speakers" button / `hearsay rediarize`). Cheap now that the diarizer is FluidAudio
    # on the ANE (~seconds), so every meeting ends with accurate labels. Gated on `refine`
    # (needs audio.wav); a missing recording or a diarizer error never blocks the stop.
    auto_refine: bool = True
    # Cosine at/above which a refined speaker's voiceprint is auto-matched to a person named
    # in a previous meeting. Conservative (a wrong cross-meeting match is worse than none).
    recognition_threshold: float = 0.6


class AudioSettings(BaseModel):
    """Full-meeting audio recording -- the single audio-retention switch."""

    # Record one timeline-accurate stereo WAV per meeting (``<folder>/audio.wav``, Me = left,
    # Them = right). It serves both the in-browser playback (with synced transcript highlighting)
    # and the post-meeting refine (which reads the Them channel). Privacy tradeoff: retains the
    # full raw audio -- set False to opt out, which also disables the refine (no recording to
    # re-diarize); delete-meeting removes the folder.
    record: bool = True


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
    diarization: DiarizationSettings = Field(default_factory=DiarizationSettings)
    audio: AudioSettings = Field(default_factory=AudioSettings)
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
        return self
