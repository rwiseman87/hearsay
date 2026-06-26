"""Shared test fixtures.

DB isolation follows the SQLAlchemy "join a session into an external transaction"
pattern: the schema is created once per session on a temp file, and each test runs
inside an outer transaction that is rolled back at teardown. ``create_savepoint``
mode turns the test's own ``commit()`` calls into SAVEPOINT releases, so commits
are exercised yet never persist across tests.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio
from sqlalchemy import create_engine as create_sync_engine
from sqlalchemy import event
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.db.engine import create_engine
from hearsay.models import Base


@pytest.fixture(scope="session")
def db_path(tmp_path_factory: pytest.TempPathFactory) -> Path:
    path = tmp_path_factory.mktemp("db") / "test.db"
    sync_engine = create_sync_engine(f"sqlite:///{path}")
    Base.metadata.create_all(sync_engine)
    sync_engine.dispose()
    return path


@pytest_asyncio.fixture
async def session(db_path: Path) -> AsyncIterator[AsyncSession]:
    engine = create_engine(f"sqlite+aiosqlite:///{db_path}")

    # pysqlite emits its own BEGIN at surprising moments, which defeats the
    # external-transaction + SAVEPOINT isolation below. Take manual control:
    # disable driver-level autobegin, then emit BEGIN ourselves on each block.
    @event.listens_for(engine.sync_engine, "connect")
    def _disable_autobegin(dbapi_connection, _record):
        dbapi_connection.isolation_level = None

    @event.listens_for(engine.sync_engine, "begin")
    def _emit_begin(conn):
        conn.exec_driver_sql("BEGIN")

    connection = await engine.connect()
    trans = await connection.begin()
    sess = AsyncSession(
        bind=connection, join_transaction_mode="create_savepoint", expire_on_commit=False
    )
    try:
        yield sess
    finally:
        await sess.close()
        await trans.rollback()
        await connection.close()
        await engine.dispose()
