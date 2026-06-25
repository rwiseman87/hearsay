from __future__ import annotations

from hearsay import __version__
from hearsay.config.settings import Settings


def test_version() -> None:
    assert __version__


def test_settings_default_db_url() -> None:
    s = Settings()
    assert s.database_url is not None
    assert s.database_url.startswith("sqlite+aiosqlite:///")
