"""Serve the built web UI (``web/dist``) over the loopback API.

The bundle is optional: if it has not been built (``cd web && npm run build``),
the API still runs (used by tests and headless ``serve``). When present, ``GET /``
returns ``index.html`` with the per-session token injected as a global plus a
minimal CSP; Vite's hashed assets are served from ``/assets``.
"""

from __future__ import annotations

import json
import secrets
from pathlib import Path

from fastapi import FastAPI, Request
from fastapi.responses import HTMLResponse
from fastapi.staticfiles import StaticFiles

from hearsay.api.context import AppContext

_SECURITY_HEADERS = {
    "X-Content-Type-Options": "nosniff",
    "X-Frame-Options": "DENY",
    "Referrer-Policy": "no-referrer",
}


def _csp(nonce: str) -> str:
    """Minimal CSP for a loopback single-page app.

    Scripts are same-origin only, except the one inline token bootstrap which
    carries a per-response nonce; ``connect-src`` also allows the loopback
    WebSocket.
    """
    return "; ".join(
        (
            "default-src 'self'",
            f"script-src 'self' 'nonce-{nonce}'",
            "style-src 'self' 'unsafe-inline'",
            "img-src 'self' data:",
            "connect-src 'self' ws://127.0.0.1:* ws://localhost:*",
            "base-uri 'none'",
            "object-src 'none'",
            "frame-ancestors 'none'",
        )
    )


def _render_index(template: str, token: str, nonce: str) -> str:
    # json.dumps yields a valid JS string literal; the token is URL-safe base64.
    bootstrap = f'<script nonce="{nonce}">window.__HEARSAY_TOKEN__={json.dumps(token)};</script>'
    marker = "</head>"
    if marker in template:
        return template.replace(marker, f"{bootstrap}{marker}", 1)
    return bootstrap + template


def mount_web(app: FastAPI, web_dir: Path) -> None:
    index_file = web_dir / "index.html"
    if not index_file.is_file():
        return  # UI not built; run API-only

    assets_dir = web_dir / "assets"
    if assets_dir.is_dir():
        app.mount("/assets", StaticFiles(directory=assets_dir), name="assets")

    @app.get("/", response_class=HTMLResponse)
    async def index(request: Request) -> HTMLResponse:
        context: AppContext = request.app.state.ctx
        nonce = secrets.token_urlsafe(16)
        html = _render_index(
            index_file.read_text(encoding="utf-8"), context.session_token, nonce
        )
        headers = {"Content-Security-Policy": _csp(nonce), **_SECURITY_HEADERS}
        return HTMLResponse(html, headers=headers)
