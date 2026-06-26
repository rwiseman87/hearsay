"""Dump the FastAPI OpenAPI schema to ``web/openapi.json`` (drives the TS codegen).

Output is deterministic (sorted keys) so ``make codegen`` produces stable diffs and
CI can fail on drift. Run: ``uv run python scripts/dump_openapi.py`` (wired as part
of ``make codegen``).
"""

from __future__ import annotations

import json
from pathlib import Path

from hearsay.api import create_app
from hearsay.config.settings import Settings

OUT = Path(__file__).resolve().parents[1] / "web" / "openapi.json"


def main() -> None:
    settings = Settings(database_url="sqlite+aiosqlite:///:memory:")
    app = create_app(settings, session_token="codegen")
    spec = app.openapi()
    OUT.write_text(json.dumps(spec, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
