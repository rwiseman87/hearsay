"""Loopback hardening: Host + Origin allowlist and bearer-token checks.

The core binds to 127.0.0.1, but loopback is not a security boundary: other local
processes and browser pages can reach it. A per-session bearer token gates every
request; the Host check blocks DNS-rebinding and the Origin check blocks cross-site
(including WebSocket) calls from other pages.
"""

from __future__ import annotations

import secrets
from urllib.parse import urlsplit

_LOOPBACK_HOSTS = frozenset({"127.0.0.1", "localhost", "::1"})


def _hostname(authority: str) -> str | None:
    # Parse "host:port" / "[::1]:port" via urlsplit's authority handling.
    return urlsplit(f"//{authority}").hostname


def host_allowed(host_header: str | None) -> bool:
    if not host_header:
        return False  # require an explicit, loopback Host
    return _hostname(host_header) in _LOOPBACK_HOSTS


def origin_allowed(origin_header: str | None) -> bool:
    if origin_header is None:
        return True  # non-browser client; the token is the gate
    return urlsplit(origin_header).hostname in _LOOPBACK_HOSTS


def token_matches(provided: str | None, expected: str) -> bool:
    if not provided:
        return False
    return secrets.compare_digest(provided, expected)


def bearer_token(authorization_header: str | None) -> str | None:
    if not authorization_header:
        return None
    scheme, _, token = authorization_header.partition(" ")
    if scheme.lower() != "bearer" or not token:
        return None
    return token
