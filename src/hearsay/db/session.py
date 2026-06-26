"""Async session factory.

``expire_on_commit=False`` keeps ORM attributes usable after ``commit()`` without
an extra round-trip (the standard async posture); transactions are managed
explicitly by the service layer (``async with session.begin(): ...``).
"""

from __future__ import annotations

from sqlalchemy.ext.asyncio import AsyncEngine, AsyncSession, async_sessionmaker


def create_sessionmaker(engine: AsyncEngine) -> async_sessionmaker[AsyncSession]:
    return async_sessionmaker(engine, expire_on_commit=False, autoflush=False)
