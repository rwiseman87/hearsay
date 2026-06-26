"""NDJSON control-channel codec for the helper<->core IPC.

Mirror of the control half of ``shared/protocol/ipc.md`` and the Swift
``HearsayIPC.ControlCodec``. One UTF-8 JSON object per line, terminated by ``\\n``:

- ``Command`` (core -> helper): ``{"id", "cmd", "args"}``
- ``Reply``   (helper -> core): ``{"id", "ok", "result"?|"error"?}`` (correlates by ``id``)
- ``Event``   (helper -> core): ``{"event", "ts", "data"}`` (unsolicited)
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

JsonObj = dict[str, Any]


class ControlError(ValueError):
    """Raised when a control line does not conform to the NDJSON contract."""


@dataclass(frozen=True, slots=True)
class Command:
    """A command from the core. ``args`` defaults to empty when the key is absent."""

    id: int
    cmd: str
    args: JsonObj = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class ReplyError:
    """Error payload carried by a failed :class:`Reply`."""

    code: str
    message: str


@dataclass(frozen=True, slots=True)
class Reply:
    """A reply to a command. Exactly one of ``result`` / ``error`` is present."""

    id: int
    ok: bool
    result: JsonObj | None = None
    error: ReplyError | None = None


@dataclass(frozen=True, slots=True)
class Event:
    """An unsolicited event. ``ts`` shares the media ``host_ts`` clock."""

    event: str
    ts: int
    data: JsonObj = field(default_factory=dict)


def _line(obj: JsonObj) -> bytes:
    # sort_keys keeps the wire bytes deterministic (matches the Swift sortedKeys
    # encoder), so golden fixtures are comparable across both implementations.
    return (json.dumps(obj, separators=(",", ":"), sort_keys=True) + "\n").encode()


def encode_command(cmd: Command) -> bytes:
    return _line({"id": cmd.id, "cmd": cmd.cmd, "args": cmd.args})


def encode_reply(reply: Reply) -> bytes:
    obj: JsonObj = {"id": reply.id, "ok": reply.ok}
    if reply.result is not None:
        obj["result"] = reply.result
    if reply.error is not None:
        obj["error"] = {"code": reply.error.code, "message": reply.error.message}
    return _line(obj)


def encode_event(event: Event) -> bytes:
    return _line({"event": event.event, "ts": event.ts, "data": event.data})


def parse_command(line: bytes | str) -> Command:
    """Parse one command line (the helper's job; used here by tests / fakes)."""
    obj = json.loads(line)
    if not isinstance(obj, dict) or "id" not in obj or "cmd" not in obj:
        raise ControlError(f"not a command line: {obj!r}")
    return Command(id=int(obj["id"]), cmd=str(obj["cmd"]), args=dict(obj.get("args", {})))


def parse_message(line: bytes | str) -> Reply | Event:
    """Parse one inbound line from the helper (a reply or an unsolicited event)."""
    obj = json.loads(line)
    if not isinstance(obj, dict):
        raise ControlError("control line is not a JSON object")
    if "event" in obj:
        return Event(
            event=str(obj["event"]), ts=int(obj.get("ts", 0)), data=dict(obj.get("data", {}))
        )
    if "id" in obj and "ok" in obj:
        err = obj.get("error")
        return Reply(
            id=int(obj["id"]),
            ok=bool(obj["ok"]),
            result=obj.get("result"),
            error=ReplyError(code=str(err["code"]), message=str(err["message"])) if err else None,
        )
    raise ControlError(f"unrecognized control line: {obj!r}")
