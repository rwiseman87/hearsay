# Development

How to set up, build, run, test, and troubleshoot the project from source.

## Prerequisites

- [`uv`](https://docs.astral.sh/uv/) — manages the Python 3.14 venv, dependencies, and runtime.
- Swift — Command Line Tools is enough to build the helper (`xcode-select --install`). Full
  Xcode is only needed for app packaging (Phase 5, deferred).
- Apple Silicon, macOS 14.4+ (Core Audio process taps).

## Setup

```sh
make sync                            # venv + base deps
uv sync --extra asr                  # transcription stack: whisper.cpp + onnxruntime VAD (torch-free)
swift build --package-path helper    # build hearsay-helper
uv run hearsay fetch-models          # download the Silero VAD model into models_dir
```

The base install (no extras) is enough for the API and the test suite; the `asr` extra adds
the ML stack needed to actually transcribe.

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
uv run hearsay live --model base --seconds 60   # join a call first
uv run hearsay live --synthetic --seconds 4     # glue smoke: tone source, no mic/TCC, no real audio
```

- `--model` overrides the ASR model for the run (`base` is fast and small; the default
  `large-v3-turbo` is higher quality but ~1.6 GB on first download).
- `--synthetic` uses the helper's tone source — useful to exercise the wiring without
  capturing real audio.
- Output lands in `outputs/recordings/<date>_live-validation/transcript.md`. `Ctrl-C` stops early.

### `hearsay capture-debug` — raw audio dump

The Phase 0 truth test: captures both streams for N seconds and writes `me.wav` / `them.wav`
(default `outputs/capture-debug/`, override with `--out`) plus drift and skew diagnostics. Use
`--synthetic` to test the IPC pipe without a mic.

## Models and dependency extras

| Extra | Adds | For |
|---|---|---|
| `asr` | pywhispercpp, onnxruntime, numpy (torch-free) | the default transcription path |
| `accel` | mlx-whisper (pulls torch) | the opt-in MLX ASR backend |
| `diarization` | pyannote-audio, onnxruntime | Phase 2 (Them diarization) |
| `bedrock` | boto3 | Phase 4 (cloud LLM, lazy-imported) |

**Swapping models.** ASR is swappable at runtime via config or the API. Set `asr.backend`
(`whispercpp` | `mlx`) and `asr.model`. A `model` may be a curated name
(`large-v3-turbo`, `large-v3`, `base`), an absolute path to a GGML file, or a backend-specific
identifier; `asr.manager.resolve_model()` maps curated names to the right form per backend.
whisper.cpp models auto-download to `models_dir` on first use. See `GET/PUT /api/asr/model`.

## Configuration

All config flows through `hearsay.config.Settings`. Common overrides (env vars are
`HEARSAY_`-prefixed; nested fields use `__`):

| Setting | Env | Default |
|---|---|---|
| Database URL | `DATABASE_URL` | `sqlite+aiosqlite:///<repo>/outputs/db/hearsay.db` |
| Output dir | `HEARSAY_OUTPUT_DIR` | `<repo>/outputs/recordings` |
| Models dir | `HEARSAY_MODELS_DIR` | `<repo>/outputs/models` |
| ASR model | `HEARSAY_ASR__MODEL` | `large-v3-turbo` |
| ASR backend | `HEARSAY_ASR__BACKEND` | `whispercpp` |
| VAD thresholds | `HEARSAY_VAD__THRESHOLD`, `HEARSAY_VAD__MIN_SILENCE_MS`, ... | see `config/settings.py` |

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
- The `Segmenter` and `MeetingService` are pure/in-memory and unit-tested directly.
- API tests use Starlette's `TestClient` with a temp DB and a fake capture (no helper).
- The pipeline is tested end to end with fakes (fake media + stub VAD + fake ASR).
- Tests that need the real ML stack are **guarded**: the Silero tests skip unless
  `onnxruntime` is installed (so they run after `uv sync --extra asr`); the helper
  integration test skips unless the Swift binary is built.

## Troubleshooting

- **macOS permission prompts on first capture.** The first `start_capture` blocks while macOS
  shows the Microphone / System Audio Recording prompts — click Allow. `live` and
  `capture-debug` give `start_capture` a 120 s timeout for this. Grants persist for the
  ad-hoc-signed helper until the next `swift build` changes its code signature.
- **First transcription is slow.** The whisper model downloads on first use (base ~150 MB,
  large-v3-turbo ~1.6 GB) to `models_dir`. Pre-fetch by running `live` once, or pick a smaller
  `--model`.
- **"helper binary not found".** Build it: `swift build --package-path helper`. The default
  path is `helper/.build/debug/hearsay-helper` (override via `HEARSAY_HELPER_PATH`).
- **Silero "model not found".** Run `uv run hearsay fetch-models`.
