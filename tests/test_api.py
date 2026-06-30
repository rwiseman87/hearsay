from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import create_engine as create_sync_engine
from starlette.websockets import WebSocketDisconnect

from hearsay.api import create_app
from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.models import Base

TOKEN = "test-token-123"
AUTH = {"Authorization": f"Bearer {TOKEN}"}


class FakeCapture:
    """Stand-in for HelperCapture; no helper, no media (so no pipeline runs)."""

    async def start(self) -> None:
        return None

    async def stop(self) -> None:
        return None

    @property
    def media(self) -> None:
        return None


@pytest.fixture
def client(tmp_path: Path) -> Iterator[TestClient]:
    db_file = tmp_path / "api.db"
    sync_engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(sync_engine)
    sync_engine.dispose()

    settings = Settings(output_dir=tmp_path / "out")
    database = Database(f"sqlite+aiosqlite:///{db_file}")
    app = create_app(settings, database=database, session_token=TOKEN, capture_factory=FakeCapture)
    with TestClient(app, base_url="http://127.0.0.1:8000") as test_client:
        yield test_client


def _start(client: TestClient, title: str = "Sync") -> dict[str, object]:
    response = client.post("/api/meetings", json={"title": title}, headers=AUTH)
    assert response.status_code == 201, response.text
    return response.json()


def test_requires_bearer_token(client: TestClient) -> None:
    assert client.get("/api/meetings").status_code == 401
    assert client.get("/api/meetings", headers={"Authorization": "Bearer wrong"}).status_code == 401


def test_rejects_non_loopback_host(client: TestClient) -> None:
    response = client.get("/api/meetings", headers={**AUTH, "host": "evil.example.com"})
    assert response.status_code == 400


def test_rejects_cross_site_origin(client: TestClient) -> None:
    response = client.get("/api/meetings", headers={**AUTH, "origin": "http://evil.example.com"})
    assert response.status_code == 403


def test_allows_loopback_origin(client: TestClient) -> None:
    response = client.get("/api/meetings", headers={**AUTH, "origin": "http://localhost:5173"})
    assert response.status_code == 200


def test_meeting_lifecycle(client: TestClient, tmp_path: Path) -> None:
    created = _start(client, "Weekly Sync")
    assert created["status"] == "recording"
    meeting_id = created["id"]
    assert (tmp_path / "out" / created["folder"]).is_dir()  # session created the folder

    listed = client.get("/api/meetings", headers=AUTH).json()
    assert listed["total"] == 1
    assert listed["items"][0]["id"] == meeting_id

    fetched = client.get(f"/api/meetings/{meeting_id}", headers=AUTH)
    assert fetched.status_code == 200

    stopped = client.post(f"/api/meetings/{meeting_id}/stop", headers=AUTH)
    assert stopped.status_code == 200
    assert stopped.json()["status"] == "finalized"


def test_start_conflicts_while_recording(client: TestClient) -> None:
    _start(client)
    conflict = client.post("/api/meetings", json={"title": "Second"}, headers=AUTH)
    assert conflict.status_code == 409


def test_segments_endpoint_empty_for_new_meeting(client: TestClient) -> None:
    meeting_id = _start(client)["id"]
    page = client.get(f"/api/meetings/{meeting_id}/segments", headers=AUTH).json()
    assert page == {"total": 0, "page": 1, "page_size": 200, "items": []}


def test_delete_meeting(client: TestClient, tmp_path: Path) -> None:
    created = _start(client)
    meeting_id = created["id"]
    folder = tmp_path / "out" / created["folder"]
    client.post(f"/api/meetings/{meeting_id}/stop", headers=AUTH)

    assert client.delete(f"/api/meetings/{meeting_id}", headers=AUTH).status_code == 204
    assert client.get(f"/api/meetings/{meeting_id}", headers=AUTH).status_code == 404
    assert not folder.exists()


def test_get_missing_meeting_404(client: TestClient) -> None:
    missing = "00000000-0000-0000-0000-000000000000"
    assert client.get(f"/api/meetings/{missing}", headers=AUTH).status_code == 404


def test_rediarize_missing_meeting_404(client: TestClient) -> None:
    missing = "00000000-0000-0000-0000-000000000000"
    assert client.post(f"/api/meetings/{missing}/rediarize", headers=AUTH).status_code == 404


def test_rediarize_without_recording_409(client: TestClient) -> None:
    meeting_id = _start(client)["id"]
    client.post(f"/api/meetings/{meeting_id}/stop", headers=AUTH)
    # No audio was captured in this test, so no them.wav exists for re-diarize to read.
    response = client.post(f"/api/meetings/{meeting_id}/rediarize", headers=AUTH)
    assert response.status_code == 409


def test_asr_models_lists_parakeet(client: TestClient) -> None:
    body = client.get("/api/asr/models", headers=AUTH).json()
    assert body["backend"] == "parakeet"
    assert [model["name"] for model in body["models"]] == ["parakeet-tdt-v3"]


def test_asr_switch_model(client: TestClient) -> None:
    response = client.put("/api/asr/model", json={"model": "base"}, headers=AUTH)
    assert response.status_code == 200
    assert response.json()["model"] == "base"
    assert client.get("/api/asr/models", headers=AUTH).json()["model"] == "base"


def test_ws_rejects_bad_token(client: TestClient) -> None:
    meeting_id = _start(client)["id"]
    url = f"/ws/meetings/{meeting_id}?token=wrong"
    with pytest.raises(WebSocketDisconnect) as excinfo, client.websocket_connect(url) as ws:
        ws.receive_text()
    assert excinfo.value.code == 1008


def test_ws_connects_to_active_meeting(client: TestClient) -> None:
    meeting_id = _start(client)["id"]
    # A valid handshake to the active meeting accepts; closing from the client is clean.
    with client.websocket_connect(f"/ws/meetings/{meeting_id}?token={TOKEN}"):
        pass


def test_ws_closes_when_meeting_not_active(client: TestClient) -> None:
    missing = "00000000-0000-0000-0000-000000000000"
    url = f"/ws/meetings/{missing}?token={TOKEN}"
    with pytest.raises(WebSocketDisconnect) as excinfo, client.websocket_connect(url) as ws:
        ws.receive_text()
    assert excinfo.value.code == 1000


def test_speakers_and_identities_empty_for_new_meeting(client: TestClient) -> None:
    meeting_id = _start(client)["id"]
    speakers = client.get(f"/api/meetings/{meeting_id}/speakers", headers=AUTH)
    assert speakers.status_code == 200
    assert speakers.json()["items"] == []
    identities = client.get("/api/identities", headers=AUTH)
    assert identities.status_code == 200
    assert identities.json() == {"total": 0, "page": 1, "page_size": 50, "items": []}


def test_rename_missing_speaker_returns_404(client: TestClient) -> None:
    meeting_id = _start(client)["id"]
    missing = "00000000-0000-0000-0000-000000000000"
    response = client.put(
        f"/api/meetings/{meeting_id}/speakers/{missing}",
        json={"display_name": "Alice"},
        headers=AUTH,
    )
    assert response.status_code == 404


def test_rename_rejects_blank_name(client: TestClient) -> None:
    meeting_id = _start(client)["id"]
    missing = "00000000-0000-0000-0000-000000000000"
    response = client.put(
        f"/api/meetings/{meeting_id}/speakers/{missing}",
        json={"display_name": "   "},  # stripped to empty -> 422 before the handler runs
        headers=AUTH,
    )
    assert response.status_code == 422
