"""Per-process application context stored on ``app.state``."""

from __future__ import annotations

from dataclasses import dataclass

from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.transcript import SessionManager


@dataclass(slots=True)
class AppContext:
    settings: Settings
    database: Database
    session_manager: SessionManager
    session_token: str
