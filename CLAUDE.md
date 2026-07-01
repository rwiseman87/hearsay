# Hearsay

Local-first meeting-note transcriber for macOS. Captures the local mic and system audio as
**separate** streams ("Me" vs "Them"), transcribes in real time, identifies the remote speakers,
and streams Markdown notes. Transcription, diarization, and the LLM run locally by default; AWS
Bedrock is configurable.

Canonical design + phased roadmap: `~/.claude/plans/i-want-to-plan-keen-lake.md`.
Resumable task tracker: `docs/TODO.md`. IPC contract: `shared/protocol/ipc.md`.
Cross-platform (macOS + Windows) Rust + Tauri direction (in progress on branch
`feat/cross-platform-rust-tauri`, not merged): `docs/architecture-cross-platform.md`.

## Architecture

Hybrid, multi-process (Apple Silicon, macOS 14.4+):

- **Swift capture helper** (`helper/`) — the ONLY process that touches guarded native APIs (Core Audio tap,
  AVAudioEngine, ScreenCaptureKit, Vision OCR, EventKit, Accessibility). A lean PCM streamer; streams PCM +
  name hints over IPC.
- **Swift sidecars** (`helper/`, FluidAudio on the Apple Neural Engine) — the audio-AI: `hearsay-live` (live
  Them diarization + Parakeet ASR), `hearsay-me` (live Me VAD + Parakeet), `hearsay-diarize` (post-meeting
  refine), `hearsay-asr` (Parakeet, used by the refine). The core spawns + feeds each over stdio.
- **Python core** (`src/hearsay/`) — orchestration (spawns the helper + sidecars, routes PCM), speaker
  attribution (clusters + cross-meeting voiceprints + manual labels), LLM notes (later phase), Markdown,
  persistence, and a loopback FastAPI + WebSocket API. Runs **no ML models** itself.
- **Web UI** — typed React frontend served by the core, shown in a WKWebView window (later phase).

The core spawns and supervises the helper; they talk over two Unix sockets (binary PCM + NDJSON
control). Capture is device-local by design and cannot be centralized — only config / output storage /
inference / telemetry could be.

## Project Structure

```
src/hearsay/
  config/settings.py   typed settings (pydantic-settings); the single config source
  enums.py             StrEnums (Stream, SampleFormat, FrameType, ...)
  log.py               common JSON logger (get_logger)
  helper/              IPC: protocol.py (FrameCodec), supervisor, media/control channels
  db/                  SQLAlchemy engine/session + Alembic migrations/
  models/              SQLAlchemy ORM models (UUID PK + created_at/updated_at)
  schemas/             Pydantic request/response models
  services/            business logic (routers stay thin)
  api/                 FastAPI routers + WebSocket
  asr/ diarization/ transcript/ export/   (+ llm/ platforms/ in later phases)
helper/                SwiftPM: hearsay-{helper,live,me,diarize,asr} executables + HearsayIPC library
shared/protocol/ipc.md IPC contract (source of truth)   ·   shared/fixtures/   golden frames
scripts/               dev tooling (gen_fixtures.py)
tests/                 pytest suite
```

## Code Conventions

- Python: strict typing everywhere — `from __future__ import annotations`, full annotations,
  `mypy --strict` clean. Ship `py.typed`.
- Python enums: use `StrEnum` for DB + JSON serialization. (The binary IPC uses integer codes; the
  mapping lives in `hearsay.helper.protocol`.)
- Pydantic schemas in `src/hearsay/schemas/`.
- Google-style docstrings. Comments only where logic is non-obvious. Do not add docstrings, comments,
  or type hints to code you did not change.
- Format + lint with `ruff` (line length 100). Prefer the standard library; add a dependency only for
  clear value.
- Frontend: TypeScript strict mode, functional components. Native Fetch API -- no Axios; one canonical
  fetch wrapper carrying the session token.
- All list endpoints return paginated responses: `{ total, page, page_size, items }`.
- Use the common logger (`hearsay.log.get_logger`); structured JSON logs.
- Swift: 4-space indent; `swift build` clean. Native frameworks only in the helper (see Swift Helper).

## Build, Test & Tooling

- **uv** for all Python env/deps/runtime. The **Makefile** is the task runner.
- Targets: `make sync`, `make test` (pytest + Swift selftest), `make typecheck`, `make lint`, `make fmt`,
  `make codegen`, `make audit`, `make licenses`, `make ci`.
- `make ci` is the gate and must stay green: ruff + `mypy --strict` + pytest + Swift `selftest` +
  `pip-audit` + license gate.
- Pin exact versions in lockfiles (`uv.lock`, `package-lock.json`). npm: `ignore-scripts=true` in `.npmrc`.

## IPC Contract

- `shared/protocol/ipc.md` is the single source of truth: a fixed 28-byte little-endian media-frame
  header + payload on `media.sock`, and NDJSON commands/events on `control.sock`.
- The Python `hearsay.helper.protocol` and Swift `HearsayIPC.FrameCodec` MUST match byte-for-byte.
- `shared/fixtures/frames.jsonl` (regenerate with `make codegen`) pins the contract; both languages
  validate against it in CI (`pytest` + `hearsay-helper selftest`). Never hand-edit fixtures.

## Swift Helper

- SwiftPM package in `helper/`: the `hearsay-helper` capture executable + the FluidAudio/ANE sidecars
  (`hearsay-{live,me,diarize,asr}`) + the `HearsayIPC` library. Deployment macOS 14.4. `make swift-build`
  builds explicit products (a bare `swift build` pulls in FluidAudio's broken CLI target).
- All TCC-guarded native work lives in the capture helper (mic, audio capture, screen recording,
  accessibility, calendar); it stays lean (no FluidAudio). The heavy CoreML dep is isolated in the sidecar
  targets, so it never touches the capture binary.
- Tests run via `hearsay-helper selftest` (works with Command Line Tools); `swift test`/XCTest needs full
  Xcode. Keep the capture helper thin and stateless where possible.

## Database

- Local-first **SQLite** via **async SQLAlchemy 2.0** (`sqlite+aiosqlite`); portable to PostgreSQL later.
- Models: `src/hearsay/models/` -- all use UUID primary keys and `created_at`/`updated_at` timestamps.
- Migrations: `src/hearsay/db/migrations/versions/` (Alembic, forward-only).
  - create: `uv run alembic revision --autogenerate -m "description"`  ·  apply: `uv run alembic upgrade head`
- Never use raw SQL strings (SQLAlchemy parameterizes). Wrap multi-step writes in explicit transactions;
  `PRAGMA journal_mode=WAL` + `busy_timeout`.

## API & Web

- Bind the core to **127.0.0.1 only** and require a **per-session bearer token** on REST + WebSocket
  (loopback is not a security boundary); enforce an Origin/Host allowlist. Minimal CSP in the webview.
- API routers are thin -- validate input, call a service, return a response. Business logic lives in
  `src/hearsay/services/`.
- Backend types are codegen'd from the OpenAPI schema; CI fails on drift.

## Testing

- pytest with async fixtures (conftest.py creates a test DB per session); use SAVEPOINT/rollback isolation.
- API tests use `httpx.AsyncClient` against the FastAPI app; service tests use in-memory fixtures.
- The diarization mapping (`order_speakers`, `assign_segment_speaker`) + voiceprint matching are pure logic --
  unit-test them directly; the pipeline, sidecar processors, and refine are tested with fakes/stubs (no ML deps).
- Run: `make test` (or `uv run pytest -x -v`).

## Audio Capture (guardrails)

- System audio: Core Audio process tap configured **global-except-self** (dodges the Teams
  per-process-silent bug; also covers browser meeting apps).
- Resample both sources to **16 kHz mono**; stamp both with ONE monotonic clock (`host_ts`). Cross-stream
  alignment is by timestamp, never by sample index.
- Zero-buffer watchdog: on sustained all-zero buffers, rebuild BOTH the tap and the aggregate device; emit
  `tap_health`. The mic is always "Me" and is never diarized.

## Speaker Identification (guardrails)

- Layers today: channel (Me/Them) + diarization (Them only, Swift/ANE) + cross-meeting voiceprints + manual
  labels. Calendar roster + active-speaker hints are the Phase-3 additions.
- Bind a diarization cluster -> name by **weighted majority vote** over many sparse hints (the Phase-3 design);
  a single wrong hint must never flip a stable binding. Manual labels lock a binding (votes cannot override).
- Active-speaker is **OCR-primary** (ScreenCaptureKit + Vision); Zoom Accessibility is opt-in. Degrade
  gracefully to "Speaker N" + manual labeling when hints are absent.

## Privacy & Security

- Local-first by default; raw-audio retention OFF by default; no telemetry by default.
- Minimize permissions: Microphone + Audio Capture + Screen Recording; Accessibility only for the opt-in
  Zoom path. Provide delete-meeting (DB rows + folder) + a retention setting; surface a recording-consent notice.
- Validate and sanitize all user input at API boundaries (Pydantic) -> 422, never let the DB raise a 500.
- No secrets in code -- use pydantic-settings / macOS Keychain (`SecretStr`); redact transcripts in logs.
- NEVER add a dependency without checking its license (must be MIT, BSD, or Apache-2.0) -- `make licenses`.
- NEVER add a dependency without checking for known CVEs (`make audit` / `pip-audit` / `npm audit`).

## Distribution

- Notarized direct download, NOT sandboxed / not App Store (the system-audio tap and Accessibility need it).
- Packaging (bundled CPython + depth-first codesign + notarize) is deferred until there is a real decision
  to distribute; for internal use, run from source.

## Dependency Decisions

- Persistence (2026-06-25): local-first SQLite via async SQLAlchemy 2.0 + Alembic. Single-user desktop app,
  so no Postgres server; the SQLAlchemy layer keeps a future Postgres/central pivot cheap.
- Python 3.14 locked (2026-06-25): originally to fit the full Python ML stack on cp314. Since the FluidAudio
  pivot (2026-06-30/07-01) that stack is gone -- ASR + diarization moved to Swift/ANE sidecars, so the Python
  core carries **no ML dependency** (numpy is the only ML-adjacent dep, to pack PCM + read them.wav). 3.14
  stays locked; the fallback ladder 3.13 -> 3.12 applies only if a dep regresses.

## Environment Variables

- DATABASE_URL: SQLAlchemy database URL (default local SQLite `sqlite+aiosqlite:///<repo>/outputs/db/hearsay.db`;
  portable to PostgreSQL for a future central deployment)
- ENVIRONMENT: development | staging | production
