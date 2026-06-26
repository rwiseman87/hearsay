"""Async SQLAlchemy engine factory.

Local-first SQLite via ``sqlite+aiosqlite`` today; the same code targets
PostgreSQL later by swapping the URL. SQLite connections are configured for
concurrent reads + a single writer (WAL), a bounded lock wait (``busy_timeout``),
and enforced foreign keys.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import event
from sqlalchemy.ext.asyncio import AsyncEngine, create_async_engine


def _install_sqlite_pragmas(engine: AsyncEngine) -> None:
    @event.listens_for(engine.sync_engine, "connect")
    def _set_pragmas(dbapi_connection: Any, _record: Any) -> None:
        cursor = dbapi_connection.cursor()
        try:
            cursor.execute("PRAGMA journal_mode=WAL")
            cursor.execute("PRAGMA busy_timeout=5000")
            cursor.execute("PRAGMA foreign_keys=ON")
        finally:
            cursor.close()


def create_engine(database_url: str, *, echo: bool = False) -> AsyncEngine:
    """Create the application's async engine.

    Args:
        database_url: A SQLAlchemy async URL (e.g. ``sqlite+aiosqlite:///path``).
        echo: Echo emitted SQL to the logger (development only).
    """
    engine = create_async_engine(
        database_url,
        echo=echo,
        pool_pre_ping=True,
        pool_size=5,
        max_overflow=10,
        pool_timeout=30,
    )
    if database_url.startswith("sqlite"):
        _install_sqlite_pragmas(engine)
    return engine
