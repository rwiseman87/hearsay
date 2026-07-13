from __future__ import annotations

from pathlib import Path

from hearsay.config.settings import Settings
from hearsay.services import probe_permissions


async def test_probe_permissions_degrades_when_helper_missing(tmp_path: Path) -> None:
    # No helper binary at the configured path -> a graceful, non-raising fallback.
    settings = Settings(helper_path=tmp_path / "no-such-helper")
    info = await probe_permissions(settings)

    assert info.helper_available is False
    assert info.helper_version is None
    assert info.microphone == "unknown"
    assert info.audio_capture == "unknown"
    assert info.screen_recording == "unknown"
    assert info.accessibility == "unknown"
    assert info.calendar == "unknown"
