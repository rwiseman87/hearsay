"""Regenerate the cross-language IPC golden fixtures.

Run: ``uv run python scripts/gen_fixtures.py`` (wired as ``make codegen``).
"""

from __future__ import annotations

import json
from pathlib import Path

from hearsay.helper.protocol import canonical_frames, fixture_record

OUT = Path(__file__).resolve().parents[1] / "shared" / "fixtures" / "frames.jsonl"


def main() -> None:
    OUT.parent.mkdir(parents=True, exist_ok=True)
    lines = [json.dumps(fixture_record(desc, frame)) for desc, frame in canonical_frames()]
    OUT.write_text("\n".join(lines) + "\n")
    print(f"wrote {len(lines)} fixtures to {OUT}")


if __name__ == "__main__":
    main()
