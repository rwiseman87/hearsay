from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from uuid import uuid4

from hearsay.export import LocalMarkdownSink, MeetingMeta, TranscriptLine


def _meta(tmp_path: Path) -> MeetingMeta:
    return MeetingMeta(
        id=uuid4(),
        title="Sync",
        started_at=datetime(2026, 6, 26, 9, 0, tzinfo=UTC),
        folder=tmp_path / "mtg",
    )


async def test_open_creates_folder_and_files(tmp_path: Path) -> None:
    meta = _meta(tmp_path)
    await LocalMarkdownSink().open(meta)
    assert (meta.folder / "transcript.md").read_text(encoding="utf-8").startswith("# Sync")
    metadata = json.loads((meta.folder / "meeting.json").read_text(encoding="utf-8"))
    assert metadata["status"] == "recording"
    assert metadata["ended_at"] is None
    assert metadata["folder"] == "mtg"


async def test_append_groups_consecutive_speaker(tmp_path: Path) -> None:
    meta = _meta(tmp_path)
    sink = LocalMarkdownSink()
    await sink.open(meta)
    await sink.append(TranscriptLine("Me", "hello", 0.0, 1.0))
    await sink.append(TranscriptLine("Me", "there", 1.0, 2.0))
    await sink.append(TranscriptLine("Them", "hi", 2.0, 3.0))

    text = (meta.folder / "transcript.md").read_text(encoding="utf-8")
    assert text.count("### ") == 2  # one header per speaker run, not per segment
    me_section = text.split("— Them")[0]
    assert "hello" in me_section and "there" in me_section
    assert text.endswith("\n")  # complete, newline-terminated blocks


async def test_header_uses_hhmmss(tmp_path: Path) -> None:
    meta = _meta(tmp_path)
    sink = LocalMarkdownSink()
    await sink.open(meta)
    await sink.append(TranscriptLine("Me", "x", 3661.0, 3662.0))
    text = (meta.folder / "transcript.md").read_text(encoding="utf-8")
    assert "### 01:01:01 — Me" in text


async def test_finalize_rewrites_in_timestamp_order(tmp_path: Path) -> None:
    meta = _meta(tmp_path)
    sink = LocalMarkdownSink()
    await sink.open(meta)
    # Live append arrives out of order (two streams finishing ASR at different times).
    await sink.append(TranscriptLine("Them", "bravo", 8.0, 9.0))
    await sink.append(TranscriptLine("Me", "alpha", 2.0, 3.0))

    # Finalize with the timestamp-sorted lines (what the pipeline pulls from the DB).
    ordered = [
        TranscriptLine("Me", "alpha", 2.0, 3.0),
        TranscriptLine("Them", "bravo", 8.0, 9.0),
    ]
    await sink.finalize(
        ordered, ended_at=datetime(2026, 6, 26, 9, 30, tzinfo=UTC), status="finalized"
    )

    text = (meta.folder / "transcript.md").read_text(encoding="utf-8")
    assert text.index("alpha") < text.index("bravo")  # rewritten in order
    assert text.index("— Me") < text.index("— Them")
    metadata = json.loads((meta.folder / "meeting.json").read_text(encoding="utf-8"))
    assert metadata["status"] == "finalized"
    assert metadata["ended_at"] is not None
