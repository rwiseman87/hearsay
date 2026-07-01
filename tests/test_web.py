from __future__ import annotations

import re
from pathlib import Path

from fastapi.testclient import TestClient
from sqlalchemy import create_engine as create_sync_engine

from hearsay.api import create_app
from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.models import Base

TOKEN = "web-token-xyz"


def _make_client(tmp_path: Path, web_dir: Path) -> TestClient:
    db_file = tmp_path / "web.db"
    sync_engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(sync_engine)
    sync_engine.dispose()
    settings = Settings(output_dir=tmp_path / "out", web_dir=web_dir)
    database = Database(f"sqlite+aiosqlite:///{db_file}")
    app = create_app(settings, database=database, session_token=TOKEN)
    return TestClient(app, base_url="http://127.0.0.1:8000")


def _build_ui(root: Path) -> Path:
    dist = root / "dist"
    (dist / "assets").mkdir(parents=True)
    (dist / "index.html").write_text(
        "<!doctype html><html><head><title>x</title></head>"
        '<body><div id="root"></div></body></html>',
        encoding="utf-8",
    )
    (dist / "assets" / "app.js").write_text("console.log('hi');\n", encoding="utf-8")
    return dist


def test_serves_index_with_injected_token_and_csp(tmp_path: Path) -> None:
    dist = _build_ui(tmp_path)
    with _make_client(tmp_path, dist) as client:
        response = client.get("/")
        assert response.status_code == 200
        body = response.text
        assert f'window.__HEARSAY_TOKEN__="{TOKEN}"' in body

        csp = response.headers["content-security-policy"]
        assert "default-src 'self'" in csp
        assert response.headers["x-content-type-options"] == "nosniff"
        assert response.headers["x-frame-options"] == "DENY"

        # The inline token script carries the same nonce the CSP header allows.
        match = re.search(r"'nonce-([^']+)'", csp)
        assert match is not None
        assert f'nonce="{match.group(1)}"' in body


def test_index_nonce_differs_per_response(tmp_path: Path) -> None:
    dist = _build_ui(tmp_path)
    with _make_client(tmp_path, dist) as client:
        first = client.get("/").headers["content-security-policy"]
        second = client.get("/").headers["content-security-policy"]
        assert first != second


def test_serves_hashed_assets(tmp_path: Path) -> None:
    dist = _build_ui(tmp_path)
    with _make_client(tmp_path, dist) as client:
        response = client.get("/assets/app.js")
        assert response.status_code == 200
        assert "console.log" in response.text


def test_api_only_when_ui_not_built(tmp_path: Path) -> None:
    missing = tmp_path / "nope" / "dist"
    with _make_client(tmp_path, missing) as client:
        assert client.get("/").status_code == 404  # no "/" route mounted
        ok = client.get("/api/meetings", headers={"Authorization": f"Bearer {TOKEN}"})
        assert ok.status_code == 200  # API still works
