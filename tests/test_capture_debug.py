"""Integration test for the full Python<->Swift capture pipe (synthetic mode).

Skipped automatically when the Swift helper has not been built. ``make test``
builds it first, so this runs in CI; a bare ``uv run pytest`` may skip it.
"""

from __future__ import annotations

import wave
from pathlib import Path

import pytest

from hearsay.config.settings import Settings
from hearsay.helper import capture_debug as cd

HELPER = Settings().helper_path

pytestmark = pytest.mark.skipif(
    not HELPER.exists(),
    reason="helper binary not built (run: swift build --package-path helper)",
)


async def test_capture_debug_synthetic_writes_wavs(tmp_path: Path) -> None:
    out = tmp_path / "cap"
    code = await cd.run(helper_path=HELPER, seconds=0.6, out_dir=out, synthetic=True)
    assert code == 0

    for name in ("me.wav", "them.wav"):
        path = out / name
        assert path.exists(), f"{name} not written"
        with wave.open(str(path), "rb") as wav:
            assert wav.getframerate() == 16_000
            assert wav.getnchannels() == 1
            assert wav.getsampwidth() == 2
            # ~0.6s at 16 kHz, minus a little startup slack.
            assert wav.getnframes() > 4_000
