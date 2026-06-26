"""In-process fan-out of live transcript events to WebSocket subscribers."""

from __future__ import annotations

import asyncio
from collections.abc import Iterator
from contextlib import contextmanager


class Broadcaster:
    def __init__(self) -> None:
        self._subscribers: set[asyncio.Queue[str]] = set()

    @contextmanager
    def subscribe(self) -> Iterator[asyncio.Queue[str]]:
        queue: asyncio.Queue[str] = asyncio.Queue()
        self._subscribers.add(queue)
        try:
            yield queue
        finally:
            self._subscribers.discard(queue)

    def publish(self, message: str) -> None:
        for queue in list(self._subscribers):
            queue.put_nowait(message)
