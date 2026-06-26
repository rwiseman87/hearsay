"""FastAPI dependencies (Annotated form, so no calls in argument defaults)."""

from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Annotated

from fastapi import Depends, HTTPException, Request, status
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.api.context import AppContext
from hearsay.api.security import bearer_token, token_matches
from hearsay.transcript import SessionManager


def get_context(request: Request) -> AppContext:
    context: AppContext = request.app.state.ctx
    return context


ContextDep = Annotated[AppContext, Depends(get_context)]


async def get_session(context: ContextDep) -> AsyncIterator[AsyncSession]:
    async with context.database.session() as session:
        yield session


def get_session_manager(context: ContextDep) -> SessionManager:
    return context.session_manager


async def require_token(request: Request, context: ContextDep) -> None:
    token = bearer_token(request.headers.get("authorization"))
    if not token_matches(token, context.session_token):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="invalid or missing bearer token",
            headers={"WWW-Authenticate": "Bearer"},
        )


SessionDep = Annotated[AsyncSession, Depends(get_session)]
ManagerDep = Annotated[SessionManager, Depends(get_session_manager)]
