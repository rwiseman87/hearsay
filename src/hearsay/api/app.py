"""FastAPI application factory.

Binds nothing itself (the caller runs uvicorn on 127.0.0.1); this assembles the
context, loopback hardening, and routers. A per-session bearer token gates the API.
"""

from __future__ import annotations

import secrets
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager

from fastapi import FastAPI, Request, Response
from fastapi.responses import JSONResponse

from hearsay import __version__
from hearsay.api import asr, meetings, speakers, ws
from hearsay.api.context import AppContext
from hearsay.api.security import host_allowed, origin_allowed
from hearsay.api.web import mount_web
from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.transcript import SessionManager
from hearsay.transcript.session import CaptureFactory

_Handler = Callable[[Request], Awaitable[Response]]


def create_app(
    settings: Settings,
    *,
    database: Database | None = None,
    session_token: str | None = None,
    capture_factory: CaptureFactory | None = None,
) -> FastAPI:
    database_url = settings.database_url
    assert database_url is not None  # filled by Settings' validator
    db = database or Database(database_url)
    token = session_token or secrets.token_urlsafe(32)
    manager = SessionManager(database=db, settings=settings, capture_factory=capture_factory)
    context = AppContext(
        settings=settings, database=db, session_manager=manager, session_token=token
    )

    @asynccontextmanager
    async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
        yield
        await manager.shutdown()
        await db.dispose()

    app = FastAPI(title="hearsay", version=__version__, lifespan=lifespan)
    app.state.ctx = context

    @app.middleware("http")
    async def enforce_loopback(request: Request, call_next: _Handler) -> Response:
        if not host_allowed(request.headers.get("host")):
            return JSONResponse({"detail": "host not allowed"}, status_code=400)
        if not origin_allowed(request.headers.get("origin")):
            return JSONResponse({"detail": "origin not allowed"}, status_code=403)
        return await call_next(request)

    app.include_router(meetings.router, prefix="/api")
    app.include_router(asr.router, prefix="/api")
    app.include_router(speakers.router, prefix="/api")
    app.include_router(ws.router)
    mount_web(app, settings.web_dir)
    return app
