# hearsay

Local-first macOS meeting-note transcriber. Captures your microphone and the system
audio output as **separate** streams ("Me" vs "Them"), transcribes in real time,
identifies the remote speakers, and streams notes to Markdown. Transcription,
diarization, and the LLM run **locally by default**; AWS Bedrock is configurable.

## Architecture

A hybrid, three-process app (Apple Silicon, macOS 14.4+):

- **Swift helper** (`helper/`) — the only process touching guarded native APIs: a Core
  Audio process tap (system audio) + `AVAudioEngine` (mic), resampled to 16 kHz mono and
  stamped with one monotonic clock. Later phases add calendar + on-screen OCR name hints.
- **Python core** (`src/hearsay/`) — VAD, ASR, diarization (speaker clustering), the transcript
  pipeline, persistence, Markdown output, and a loopback FastAPI + WebSocket API. Spawns and
  supervises the helper.
- **Web UI** — a typed React frontend (Vite + React 19 + TanStack Query) served by the core at
  `/` with the session token injected; the native WKWebView window is a later phase.

The helper and core talk over two Unix sockets; the binary/NDJSON contract is in
[`shared/protocol/ipc.md`](shared/protocol/ipc.md), pinned by golden fixtures that both
languages validate in CI.

```mermaid
flowchart LR
  Helper["Swift helper\nmic + system tap -> 16 kHz mono PCM"]
  Core["Python core\nVAD -> ASR + diarization -> DB + transcript.md + WS"]
  UI["Web UI (React)\nstart/stop + live transcript + rename speakers"]
  Core -- "spawns + supervises" --> Helper
  Helper -- "media.sock (PCM) + control.sock (NDJSON)" --> Core
  Core -- "REST + WebSocket (127.0.0.1 + token)" --> UI
```

## Status

**Phases 1-2 work end to end; the on-device audio-AI now runs in Swift sidecars on the Apple
Neural Engine (FluidAudio).** Capture (Me/Them separation) feeds the `hearsay-live` (Them:
streaming diarization + Parakeet ASR) and `hearsay-me` (Me: streaming VAD + Parakeet) sidecars →
SQLite + live `transcript.md` + the loopback REST/WebSocket API + the React UI. Remote speakers
are labeled **Speaker 1..N** live and refined by a whole-track pass at stop (`hearsay-diarize`),
which also recognizes returning people by voiceprint; **rename them to real people** in the UI —
names persist and carry across meetings. Me is the mic channel and is never diarized. The Python
core runs no ML models. See [`docs/TODO.md`](docs/TODO.md) for the phase-by-phase tracker.

## Quickstart

Prereqs: [`uv`](https://docs.astral.sh/uv/) and Swift (Command Line Tools is enough).

```sh
make sync                                  # create the venv + all deps (Python 3.14; numpy is the only ML-adjacent dep)
make swift-build                           # build the capture helper + the FluidAudio/ANE sidecars
(cd web && npm ci && npm run build)        # build the React UI bundle (web/dist), served by the core
```

Run the real pipeline and watch live transcripts (the on-device validation path):

```sh
uv run hearsay live --model base --seconds 60   # join a call first; talk + play remote audio
```

Or run the API server and open the UI:

```sh
uv run hearsay serve                 # prints a loopback URL with the per-session ?token=
```

Open the printed `http://127.0.0.1:<port>/?token=...` link — the core serves the built UI with
the token injected. For frontend dev with hot reload: `cd web && npm run dev` against
`uv run hearsay serve --port 8137` (Vite proxies `/api` + `/ws`).

## Documentation

| Doc | Contents |
|---|---|
| [docs/architecture.md](docs/architecture.md) | The three-process design and a module-by-module tour of the Python core — what each piece does and why. |
| [docs/pipeline.md](docs/pipeline.md) | The real-time transcription data flow: capture -> IPC -> Swift sidecars (diarize + ASR on the ANE) -> DB + `transcript.md` + WebSocket. |
| [docs/api.md](docs/api.md) | REST + WebSocket reference: auth model, endpoints, request/response examples. |
| [docs/development.md](docs/development.md) | Setup, `make` targets, running (`serve`/`live`), model management, testing, troubleshooting. |
| [shared/protocol/ipc.md](shared/protocol/ipc.md) | The helper <-> core IPC contract (source of truth). |
| [docs/TODO.md](docs/TODO.md) | Durable, resumable progress tracker. |

## Repo layout

```
src/hearsay/
  config/      typed settings (pydantic-settings); the single config source
  enums.py     StrEnums for DB + JSON serialization
  log.py       common structured (JSON) logger
  helper/      core-side IPC: frame codec, control/media channels, supervisor, capture-debug
  db/ models/  async SQLAlchemy engine/session + ORM models + Alembic migrations
  schemas/     Pydantic request/response models (the API boundary)
  services/    business logic (routers stay thin)
  transcript/  orchestration: MeetingSession, capture seam, pipeline, live Me/Them sidecars, refine
  asr/         ASRBackend seam + Parakeet sidecar backend (used by the refine)
  diarization/ offline diarizer seam (Swift hearsay-diarize) + cross-meeting voiceprints
  export/      output Sink seam + local Markdown writer
  api/         FastAPI app, routers, WebSocket, loopback security, DI
  cli.py       the `hearsay` command (serve, live, rediarize, capture-debug)
helper/        SwiftPM: hearsay-{helper,diarize,asr,live,me} executables + HearsayIPC library
web/           React UI (Vite + TS): typed fetch client, TanStack Query, OpenAPI-generated types
shared/        IPC contract (ipc.md) + golden frame fixtures
tests/         pytest suite (SAVEPOINT-isolated DB tests; guarded on-device tests)
```

## Conventions

uv for everything; `mypy --strict` + `ruff` clean; `StrEnum` for DB/JSON; thin routers +
a service layer; Pydantic schemas; permissive licenses only (MIT/BSD/Apache, gated in CI);
`pip-audit` clean. The `Makefile` is the task runner and `make ci` is the gate. Full
conventions are in [`CLAUDE.md`](CLAUDE.md).
