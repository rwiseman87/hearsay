"""Async NDJSON control channel over the connected ``control.sock`` stream.

Owns the read loop: correlates replies to in-flight commands by ``id`` and routes
unsolicited events to a queue. The core side sends commands via :meth:`call`.
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
from collections.abc import AsyncIterator

from hearsay.helper.control import (
    Command,
    Event,
    JsonObj,
    Reply,
    encode_command,
    parse_message,
)
from hearsay.log import get_logger


class ControlChannel:
    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self._reader = reader
        self._writer = writer
        self._ids = itertools.count(1)
        self._pending: dict[int, asyncio.Future[Reply]] = {}
        self._events: asyncio.Queue[Event] = asyncio.Queue()
        self._log = get_logger("hearsay.control")
        self._task: asyncio.Task[None] | None = None

    def start(self) -> None:
        """Begin the background read loop (idempotent)."""
        if self._task is None:
            self._task = asyncio.create_task(self._read_loop())

    async def _read_loop(self) -> None:
        try:
            while True:
                line = await self._reader.readline()
                if not line:
                    break  # EOF
                try:
                    msg = parse_message(line)
                except ValueError:
                    self._log.warning("dropping malformed control line: %r", line)
                    continue
                if isinstance(msg, Reply):
                    fut = self._pending.pop(msg.id, None)
                    if fut is not None and not fut.done():
                        fut.set_result(msg)
                else:
                    await self._events.put(msg)
        finally:
            err = ConnectionError("control channel closed")
            for fut in self._pending.values():
                if not fut.done():
                    fut.set_exception(err)
            self._pending.clear()

    async def call(self, cmd: str, args: JsonObj | None = None, *, timeout: float = 5.0) -> Reply:
        """Send a command and await its correlated reply."""
        cid = next(self._ids)
        fut: asyncio.Future[Reply] = asyncio.get_running_loop().create_future()
        self._pending[cid] = fut
        self._writer.write(encode_command(Command(cid, cmd, args or {})))
        await self._writer.drain()
        try:
            return await asyncio.wait_for(fut, timeout)
        finally:
            self._pending.pop(cid, None)

    async def next_event(self, *, timeout: float | None = None) -> Event:
        if timeout is None:
            return await self._events.get()
        return await asyncio.wait_for(self._events.get(), timeout)

    async def wait_for_event(self, name: str, *, timeout: float = 10.0) -> Event:
        """Wait for the next event named ``name``, discarding others meanwhile."""
        loop = asyncio.get_running_loop()
        deadline = loop.time() + timeout
        while True:
            remaining = deadline - loop.time()
            if remaining <= 0:
                raise TimeoutError(f"timed out waiting for '{name}' event")
            ev = await asyncio.wait_for(self._events.get(), remaining)
            if ev.event == name:
                return ev
            self._log.debug("ignoring %s event while waiting for %s", ev.event, name)

    async def events(self) -> AsyncIterator[Event]:
        while True:
            yield await self._events.get()

    async def aclose(self) -> None:
        if self._task is not None:
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._task
            self._task = None
        self._writer.close()
        with contextlib.suppress(OSError):
            await self._writer.wait_closed()
