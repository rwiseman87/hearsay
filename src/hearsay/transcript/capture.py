"""Capture seam for a meeting.

``MeetingSession`` drives capture through the small :class:`Capture` protocol so
the lifecycle is testable without spawning the Swift helper. ``HelperCapture`` is
the real implementation; the media-consumption pipeline (VAD + ASR) attaches here
in a later phase.
"""

from __future__ import annotations

import contextlib
import shutil
import tempfile
from pathlib import Path
from typing import Protocol

from hearsay.helper.media_channel import MediaChannel
from hearsay.helper.protocol import SAMPLE_RATE
from hearsay.helper.supervisor import HelperSupervisor
from hearsay.log import get_logger

_log = get_logger("hearsay.capture")

# The first real start_capture blocks the helper on the macOS TCC prompts, so
# allow ample time (matches capture-debug).
_START_CAPTURE_TIMEOUT = 120.0


class Capture(Protocol):
    """The minimal capture surface a meeting needs."""

    async def start(self) -> None: ...

    async def stop(self) -> None: ...

    @property
    def media(self) -> MediaChannel | None:
        """The live media channel once started, else None (e.g. test fakes)."""
        ...


class HelperCapture:
    """Spawn the Swift helper, start the two-stream tap, and tear it all down."""

    def __init__(
        self,
        *,
        helper_path: Path,
        synthetic: bool = False,
        tap_mode: str = "global_except_self",
        sample_rate: int = SAMPLE_RATE,
    ) -> None:
        self._helper_path = helper_path
        self._synthetic = synthetic
        self._tap_mode = tap_mode
        self._sample_rate = sample_rate
        self._supervisor: HelperSupervisor | None = None
        self._run_dir: Path | None = None

    @property
    def media(self) -> MediaChannel | None:
        return self._supervisor.media if self._supervisor is not None else None

    async def start(self) -> None:
        self._run_dir = Path(tempfile.mkdtemp(prefix="hearsay-run-"))
        self._supervisor = HelperSupervisor(
            helper_path=self._helper_path, run_dir=self._run_dir, synthetic=self._synthetic
        )
        await self._supervisor.start()
        control = self._supervisor.control
        assert control is not None  # set by a successful start()
        await control.call(
            "start_capture",
            {"tap_mode": self._tap_mode, "sample_rate": self._sample_rate},
            timeout=_START_CAPTURE_TIMEOUT,
        )

    async def stop(self) -> None:
        if self._supervisor is not None:
            control = self._supervisor.control
            if control is not None:
                with contextlib.suppress(TimeoutError, ConnectionError, OSError):
                    await control.call("stop_capture")
            await self._supervisor.stop()
            self._supervisor = None
        if self._run_dir is not None:
            shutil.rmtree(self._run_dir, ignore_errors=True)
            self._run_dir = None
