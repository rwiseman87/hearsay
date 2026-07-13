"""Live TCC-permission probe: briefly spawn the capture helper and read its snapshot.

Unlike the editable settings sections, permissions are live OS state rather than a stored
preference. This spawns the helper, reads its ``hello`` (build version) and its
``check_permissions`` reply, then shuts it down. When the helper binary is absent or does
not respond the probe degrades to ``helper_available=False`` with every permission
``unknown`` -- the panel still renders, just without live status.
"""

from __future__ import annotations

import contextlib
import tempfile
from pathlib import Path
from typing import Any

from hearsay.config.settings import Settings
from hearsay.helper.supervisor import HelperSupervisor, SupervisorError
from hearsay.log import get_logger
from hearsay.schemas import PermissionsInfo

_log = get_logger("hearsay.permissions")

_UNKNOWN = "unknown"
_VALID_STATES = frozenset({"granted", "denied", "undetermined"})
_PROBE_TIMEOUT = 10.0


def _unavailable() -> PermissionsInfo:
    return PermissionsInfo(
        helper_available=False,
        helper_version=None,
        microphone=_UNKNOWN,
        audio_capture=_UNKNOWN,
        screen_recording=_UNKNOWN,
        accessibility=_UNKNOWN,
        calendar=_UNKNOWN,
    )


def _state(snapshot: dict[str, Any], key: str) -> str:
    """Map one helper snapshot entry to a known state, defaulting to ``unknown``."""
    value = snapshot.get(key)
    return value if isinstance(value, str) and value in _VALID_STATES else _UNKNOWN


async def probe_permissions(settings: Settings) -> PermissionsInfo:
    """Spawn the helper briefly, read live TCC permission status, and shut it down.

    Never raises: a missing or unresponsive helper yields a ``helper_available=False``
    snapshot with every permission ``unknown``.
    """
    if not settings.helper_path.exists():
        _log.info("permissions probe: helper binary missing at %s", settings.helper_path)
        return _unavailable()

    run_dir = Path(tempfile.mkdtemp(prefix="hearsay-perms-"))
    sup = HelperSupervisor(helper_path=settings.helper_path, run_dir=run_dir)
    try:
        await sup.start(timeout=_PROBE_TIMEOUT)
        assert sup.control is not None  # set by a successful start()
        reply = await sup.control.call("check_permissions", timeout=_PROBE_TIMEOUT)
        snapshot = reply.result if reply.ok and reply.result is not None else {}
        version = sup.hello.data.get("helper_version") if sup.hello is not None else None
        return PermissionsInfo(
            helper_available=True,
            helper_version=version if isinstance(version, str) else None,
            microphone=_state(snapshot, "microphone"),
            audio_capture=_state(snapshot, "audio_capture"),
            screen_recording=_state(snapshot, "screen_recording"),
            accessibility=_state(snapshot, "accessibility"),
            calendar=_state(snapshot, "calendar"),
        )
    except (SupervisorError, TimeoutError, ConnectionError, OSError) as exc:
        _log.warning("permissions probe failed: %s", exc)
        return _unavailable()
    finally:
        await sup.stop()
        for sock in (run_dir / "control.sock", run_dir / "media.sock"):
            sock.unlink(missing_ok=True)
        with contextlib.suppress(OSError):
            run_dir.rmdir()
