from __future__ import annotations

from pathlib import Path

from alembic import command
from alembic.config import Config
from sqlalchemy import create_engine, inspect

REPO_ROOT = Path(__file__).resolve().parents[1]


def test_migrations_upgrade_creates_schema(tmp_path: Path) -> None:
    db = tmp_path / "m.db"
    cfg = Config(str(REPO_ROOT / "alembic.ini"))
    cfg.set_main_option("script_location", str(REPO_ROOT / "src" / "hearsay" / "db" / "migrations"))
    cfg.set_main_option("sqlalchemy.url", f"sqlite+aiosqlite:///{db}")
    command.upgrade(cfg, "head")

    engine = create_engine(f"sqlite:///{db}")
    try:
        inspector = inspect(engine)
        tables = set(inspector.get_table_names())
        segment_indexes = {ix["name"] for ix in inspector.get_indexes("segments")}
        segment_columns = {col["name"] for col in inspector.get_columns("segments")}
    finally:
        engine.dispose()

    assert {"meetings", "segments", "clusters", "identities"}.issubset(tables)
    assert "ix_segments_meeting_start" in segment_indexes
    assert "cluster_id" in segment_columns
