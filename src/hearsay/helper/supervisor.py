"""Spawn and supervise the Swift capture helper over the two IPC sockets.

The core owns (listens on) ``control.sock`` + ``media.sock`` inside a run dir; the
helper connects to both as a client (see ``shared/protocol/ipc.md``).

Phase 0 scope: create the run dir, listen, spawn, accept both connections, await
the ``hello`` event, and shut down cleanly. Respawn-with-backoff (ipc.md step 5)
layers on in Phase 1 with the long-lived ``MeetingSession``.
"""

from __future__ import annotations

import asyncio
import contextlib
from pathlib import Path

from hearsay.helper.control import Event
from hearsay.helper.control_channel import ControlChannel
from hearsay.helper.media_channel import MediaChannel
from hearsay.log import get_logger

_log = get_logger("hearsay.supervisor")

_Conn = tuple[asyncio.StreamReader, asyncio.StreamWriter]


class SupervisorError(RuntimeError):
    """Raised when the helper cannot be spawned or fails to connect."""


class _OneShotServer:
    """A Unix-socket server that captures the first inbound connection."""

    def __init__(self, path: Path) -> None:
        self._path = path
        self._server: asyncio.Server | None = None
        self._connected: asyncio.Future[_Conn] | None = None

    async def listen(self) -> None:
        self._connected = asyncio.get_running_loop().create_future()
        self._server = await asyncio.start_unix_server(self._on_conn, path=str(self._path))

    async def _on_conn(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        if self._connected is not None and not self._connected.done():
            self._connected.set_result((reader, writer))

    async def accept(self, *, timeout: float) -> _Conn:
        if self._connected is None:
            raise SupervisorError("server is not listening")
        return await asyncio.wait_for(self._connected, timeout)

    async def close(self) -> None:
        if self._server is not None:
            self._server.close()
            # wait_closed() blocks until every accepted connection's transport is
            # closed; bound it so a lingering peer can never hang teardown.
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(self._server.wait_closed(), 2.0)
            self._server = None


class HelperSupervisor:
    def __init__(self, *, helper_path: Path, run_dir: Path, synthetic: bool = False) -> None:
        self.helper_path = helper_path
        self.run_dir = run_dir
        self.synthetic = synthetic
        self.proc: asyncio.subprocess.Process | None = None
        self.control: ControlChannel | None = None
        self.media: MediaChannel | None = None
        self.hello: Event | None = None
        self._media_writer: asyncio.StreamWriter | None = None
        self._servers: list[_OneShotServer] = []

    async def start(self, *, timeout: float = 10.0) -> None:
        if not self.helper_path.exists():
            raise SupervisorError(
                f"helper binary not found at {self.helper_path}; "
                f"build it with `swift build --package-path helper`"
            )
        self.run_dir.mkdir(parents=True, exist_ok=True)
        control_path = self.run_dir / "control.sock"
        media_path = self.run_dir / "media.sock"
        for path in (control_path, media_path):
            path.unlink(missing_ok=True)

        control_srv = _OneShotServer(control_path)
        media_srv = _OneShotServer(media_path)
        await control_srv.listen()
        await media_srv.listen()
        self._servers = [control_srv, media_srv]

        argv = [str(self.helper_path), "serve", "--socket-dir", str(self.run_dir)]
        if self.synthetic:
            argv.append("--synthetic")
        _log.info("spawning helper: %s", " ".join(argv))
        self.proc = await asyncio.create_subprocess_exec(*argv)

        try:
            control_reader, control_writer = await control_srv.accept(timeout=timeout)
            media_reader, media_writer = await media_srv.accept(timeout=timeout)
        except TimeoutError as exc:
            await self.stop()
            raise SupervisorError("helper did not connect to both sockets in time") from exc

        self.control = ControlChannel(control_reader, control_writer)
        self.control.start()
        self.media = MediaChannel(media_reader)
        self.media.start()
        # The media stream is uni-directional (helper -> core); we never write to it
        # but must own its writer so teardown can close the transport (otherwise the
        # media server's wait_closed() would hang on the still-open connection).
        self._media_writer = media_writer

        self.hello = await self.control.wait_for_event("hello", timeout=timeout)
        _log.info("helper hello: %s", self.hello.data)

    async def stop(self, *, timeout: float = 5.0) -> None:
        """Best-effort graceful shutdown, then ensure the process is gone."""
        if self.control is not None:
            with contextlib.suppress(TimeoutError, ConnectionError, OSError):
                await self.control.call("shutdown", timeout=2.0)
            await self.control.aclose()
            self.control = None
        if self.media is not None:
            await self.media.aclose()
            self.media = None
        if self._media_writer is not None:
            self._media_writer.close()
            with contextlib.suppress(OSError):
                await self._media_writer.wait_closed()
            self._media_writer = None
        if self.proc is not None:
            if self.proc.returncode is None:
                try:
                    await asyncio.wait_for(self.proc.wait(), timeout=timeout)
                except TimeoutError:
                    self.proc.terminate()
                    try:
                        await asyncio.wait_for(self.proc.wait(), timeout=2.0)
                    except TimeoutError:
                        self.proc.kill()
            self.proc = None
        for server in self._servers:
            await server.close()
        self._servers = []

    async def __aenter__(self) -> HelperSupervisor:
        await self.start()
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.stop()
