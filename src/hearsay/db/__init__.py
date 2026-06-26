"""Database access layer: async engine + session factory.

``Database`` bundles an engine with its sessionmaker so the rest of the app holds
one object (in ``app.state``) and pulls sessions from it; tests build one against
a temp file. Schema lives in ``hearsay.models``; migrations in ``migrations/``.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager

from sqlalchemy.ext.asyncio import AsyncEngine, AsyncSession, async_sessionmaker

from hearsay.db.engine import create_engine
from hearsay.db.session import create_sessionmaker

__all__ = ["Database", "create_engine", "create_sessionmaker"]


class Database:
    def __init__(self, database_url: str, *, echo: bool = False) -> None:
        self.engine: AsyncEngine = create_engine(database_url, echo=echo)
        self.sessionmaker: async_sessionmaker[AsyncSession] = create_sessionmaker(self.engine)

    @asynccontextmanager
    async def session(self) -> AsyncIterator[AsyncSession]:
        """Yield a session, rolling back and closing on exit."""
        async with self.sessionmaker() as session:
            yield session

    async def dispose(self) -> None:
        await self.engine.dispose()
