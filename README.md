# hearsay

Local-first macOS meeting-note transcriber. Captures your microphone and the system
audio output as **separate** streams ("Me" vs "Them"), transcribes in real time,
identifies the remote speakers, and streams notes to Markdown. Transcription,
diarization, and the LLM run **locally by default**; AWS Bedrock is configurable.

## Architecture

A hybrid, three-process app (Apple Silicon, macOS 14.4+):

- **Swift helper** (`helper/`) — the only process touching guarded native APIs: Core
  Audio process tap (system audio) + AVAudioEngine (mic), plus calendar/OCR name hints.
- **Python core** (`src/hearsay/`) — ASR, diarization, speaker-attribution fusion, LLM
  notes, Markdown output, persistence, and a local (loopback) FastAPI + WebSocket API.
- **Web UI in a native window** — a typed React frontend served by the core (later phase).

The helper and core talk over two Unix sockets; the contract is in
[`shared/protocol/ipc.md`](shared/protocol/ipc.md) and pinned by golden fixtures that
both languages validate in CI. The full design and phased roadmap live in the plan at
`~/.claude/plans/i-want-to-plan-keen-lake.md`.

## Status

Phase 0 foundation is in place and green: Python scaffold (3.14, `mypy --strict`),
the cross-language IPC `FrameCodec`, and the `make ci` gate. Next: the Swift audio
capture and the Python capture-debug reader (verified on-device).

## Development

Prereqs: [`uv`](https://docs.astral.sh/uv/) and Swift (Command Line Tools is enough for
the helper; full Xcode is needed only for app packaging later).

```sh
make help        # list targets
make sync        # create the venv and install deps
make ci          # lint + type-check + tests (Python & Swift) + CVE audit + license gate
make test        # Python tests + Swift cross-language self-test
make codegen     # regenerate shared/fixtures from the codec
```

## Layout

```
src/hearsay/        Python core (config, enums, log, helper/protocol, db, models, schemas, services, api)
helper/             SwiftPM: hearsay-helper executable + HearsayIPC library
shared/protocol/    IPC contract (ipc.md) + golden fixtures
scripts/            dev tooling (fixture generation)
tests/              pytest suite
```
