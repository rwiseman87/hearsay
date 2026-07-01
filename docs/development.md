# Development

How to set up, build, run, test, and troubleshoot the project from source.

## Prerequisites

- [`uv`](https://docs.astral.sh/uv/) — manages the Python 3.14 venv, dependencies, and runtime.
- Swift — Command Line Tools is enough to build the helper (`xcode-select --install`). Full
  Xcode is only needed for app packaging (Phase 5, deferred).
- Apple Silicon, macOS 14.4+ (Core Audio process taps).

## Setup

```sh
make sync                                  # venv + all deps (numpy is the only ML-adjacent dep)
make swift-build                           # build hearsay-helper + the FluidAudio/ANE sidecars
```

`make sync` installs the full runtime: after the pivot the Python core runs no ML models, so
numpy (to pack PCM for the sidecars, build the stereo audio.wav, and read its Them channel in the
refine) is the only ML-adjacent dependency, and it is a base dependency. All ASR + diarization
runs in the Swift sidecars, whose CoreML models auto-download on first use.

## Make targets

The `Makefile` is the task runner.

| Target | What it does |
|---|---|
| `make sync` | Create the venv and install base + dev dependencies. |
| `make test` | Build the helper, run pytest, then the Swift cross-language self-test. |
| `make typecheck` | `mypy --strict` over `src` + `scripts`. |
| `make lint` / `make fmt` | `ruff` check / format. |
| `make codegen` | Regenerate the golden IPC fixtures **and** the OpenAPI schema + web TS types. |
| `make audit` | `pip-audit` CVE scan. |
| `make licenses` | Fail on any copyleft dependency (permissive-only gate). |
| `make ci` | The Python + Swift gate: lint + typecheck + tests + audit + licenses. Must stay green. |
| `make web-ci` | The web gate: `npm ci` + OpenAPI→TS drift check + `tsc` + `vite build`. |
| `make web-build` / `make web-typecheck` | Build the UI bundle (`web/dist`) / type-check it. |

## Running

### `hearsay serve` — the API server

```sh
uv run hearsay serve            # auto-picks a free port; prints URL + per-session token
uv run hearsay serve --port 8137
```

Binds `127.0.0.1` and prints a bearer token (see [api.md](api.md)). This is what the web UI
loads.

### Web UI (`web/`)

Vite + React + TypeScript (strict). The core serves the built bundle at `/` with the session
token injected, so the production flow is build-then-serve:

```sh
cd web && npm ci          # install pinned deps (one time)
npm run build             # -> web/dist (served by `hearsay serve`)
```

For frontend development with hot reload, run the core on a fixed port and Vite in front of it
(Vite proxies `/api` + `/ws` to the core; see `web/vite.config.ts`):

```sh
uv run hearsay serve --port 8137     # terminal 1
cd web && npm run dev                 # terminal 2 -> http://localhost:5173/?token=<token>
```

API TypeScript types are generated from the backend's OpenAPI schema — never hand-edited:
`make codegen` runs `scripts/dump_openapi.py` (→ `web/openapi.json`) then `openapi-typescript`
(→ `web/src/api/schema.ts`). `make web-codegen-check` (and the CI `web` job) fail if either
drifts from the backend. The WebSocket `TranscriptEvent` is not in the OpenAPI schema, so it is
hand-mirrored in `web/src/api/ws.ts`.

### `hearsay live` — the validation harness

Runs the **exact** production pipeline (SessionManager -> helper -> VAD -> ASR -> DB +
`transcript.md`) directly from the CLI and prints live `[partial]` / `[final]` lines. This is
the on-device end-to-end test.

```sh
uv run hearsay live --seconds 60                # join a call first
uv run hearsay live --synthetic --seconds 4     # glue smoke: tone source, no mic/TCC, no real audio
```

- `--synthetic` uses the helper's tone source — useful to exercise the wiring without
  capturing real audio.
- Output lands in `outputs/recordings/<date>_live-validation/transcript.md`. `Ctrl-C` stops early.

### `hearsay capture-debug` — raw audio dump

The Phase 0 truth test: captures both streams for N seconds and writes `me.wav` / `them.wav`
(default `outputs/capture-debug/`, override with `--out`) plus drift and skew diagnostics. Use
`--synthetic` to test the IPC pipe without a mic.

## Models and dependency extras

ASR, diarization, and voiceprints all run in the Swift sidecars on the ANE, so the Python core
carries **no ML dependency**. numpy (packing PCM for the sidecars, building the stereo audio.wav,
reading its Them channel in the refine) is the only ML-adjacent dep and is a **base** dependency,
so `make sync` is the whole runtime. The one optional extra is:

| Extra | Adds | For |
|---|---|---|
| `bedrock` | boto3 | Phase 4 (cloud LLM, lazy-imported) |

**ASR + diarization models.** These live in the Swift sidecars (FluidAudio on the ANE): Parakeet
TDT for ASR (`hearsay-asr` / `hearsay-me` / `hearsay-live`), pyannote community-1 as CoreML for
the offline diarizer (`hearsay-diarize`). Their CoreML models are ungated and auto-download +
compile on first use — no fetch step, no HF token. Parakeet ships a single bundled model, so
there is no model picker or ASR config.

**Diarization.** The live Them stream is labeled Speaker 1..N by the `hearsay-live` sidecar. The
post-meeting refine (`HEARSAY_DIARIZATION__REFINE`, default on) re-diarizes the whole Them track
for better accuracy and recognizes returning people by voiceprint; it runs automatically at stop
(`HEARSAY_DIARIZATION__AUTO_REFINE`) and on demand via the "Refine speakers" button /
`hearsay rediarize`. Rename a speaker in the UI (or `PUT /api/meetings/{id}/speakers/{cluster_id}`)
to bind a name that persists, is carried across a re-diarize, and is suggested next meeting.

## Configuration

All config flows through `hearsay.config.Settings`. Common overrides (env vars are
`HEARSAY_`-prefixed; nested fields use `__`):

| Setting | Env | Default |
|---|---|---|
| Database URL | `DATABASE_URL` | `sqlite+aiosqlite:///<repo>/outputs/db/hearsay.db` |
| Output dir | `HEARSAY_OUTPUT_DIR` | `<repo>/outputs/recordings` |
| Record meeting audio (`audio.wav`) | `HEARSAY_AUDIO__RECORD` | `true` |
| Run the post-meeting refine | `HEARSAY_DIARIZATION__REFINE` | `true` |
| Auto-refine at finalize | `HEARSAY_DIARIZATION__AUTO_REFINE` | `true` |

**Audio recording + playback.** When `audio.record` is on (default), each meeting records one
timeline-accurate **stereo** `audio.wav` (Me = left channel, Them = right). This single file serves
both playback — the UI plays it with the transcript highlighting in sync (click a line to seek) —
and the post-meeting refine, which reads its Them channel. Privacy tradeoff: it retains the full
raw audio; set `HEARSAY_AUDIO__RECORD=false` to opt out (which also disables the refine, since
there is no recording to re-diarize; delete-meeting removes the folder). Served by
`GET /api/meetings/{id}/audio`.

When run from source, all runtime data (recordings, the SQLite DB, downloaded models) lives
under the repo's `outputs/` (gitignored). Override any path with the env vars above.

## Testing

```sh
uv run pytest -q          # Python tests
make test                 # Python + Swift cross-language self-test
```

- DB tests use SAVEPOINT/nested-transaction isolation (`tests/conftest.py`): the schema is
  created once on a temp file and each test runs inside an outer transaction rolled back at
  teardown, so even code that commits stays isolated.
- `MeetingService` and `SpeakerService` are in-memory and unit-tested directly.
- API tests use Starlette's `TestClient` with a temp DB and a fake capture (no helper).
- The pipeline is tested end to end with fake media + fake sidecar processors: each stream's
  PCM routes to its processor, and the Them track is recorded.
- The sidecar processors are tested against their NDJSON contract, including the broken-pipe
  resilience path; the offline diarizer + refine use a stub diarizer (no ML deps).
- The helper integration test skips unless the Swift binary is built.

## Troubleshooting

- **macOS permission prompts on first capture.** The first `start_capture` blocks while macOS
  shows the Microphone / System Audio Recording prompts — click Allow. `live` and
  `capture-debug` give `start_capture` a 120 s timeout for this. Grants persist for the
  ad-hoc-signed helper until the next `swift build` changes its code signature.
- **First transcription is slow.** The FluidAudio CoreML models (Parakeet, the diarizer)
  download and compile on first use, and each sidecar takes ~10 s to load Parakeet at startup.
  Warm utterances are fast. Pre-warm by running `live` once.
- **"helper binary not found" / a sidecar's live transcription is off.** Build them all:
  `make swift-build`. The default capture-helper path is `helper/.build/debug/hearsay-helper`
  (override via `HEARSAY_HELPER_PATH`); the sidecars are located as its siblings.
- **Speakers over- or under-merge.** The live labels are approximate; run the refine ("Refine
  speakers" / `hearsay rediarize <id>`) for a more accurate whole-track re-diarization. Manual
  renames are carried across a re-diarize.
