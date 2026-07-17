# Development

How to set up, build, run, test, and troubleshoot the project from source.

## Prerequisites

- A Rust toolchain ([rustup](https://rustup.rs/)) — builds the core and all crates.
- Swift — Command Line Tools is enough to build the helper + sidecars (`xcode-select --install`).
  Full Xcode is only needed for `.app`/`.dmg` packaging.
- Node 22 — builds the React UI.
- Apple Silicon, macOS 14.4+ (Core Audio process taps).

## Setup

```sh
make swift-build                           # build hearsay-helper + the FluidAudio/ANE sidecars
(cd web && npm ci && npm run build)        # build the UI bundle (web/dist), served by the core
```

No interpreter to install: the core is a single Rust binary. The Swift sidecars' CoreML models
auto-download on first use; the whisper refine model is a separate download (see Models below).

## Make targets

The `Makefile` is the task runner.

| Target | What it does |
|---|---|
| `make swift-build` | Build the capture helper + the FluidAudio/ANE sidecars (explicit products). |
| `make rust-build` / `make rust-test` | Build / test the Rust workspace. |
| `make test` | Build the helper, run the Swift cross-language self-test, then `cargo test`. |
| `make lint` / `make fmt` | `clippy -D warnings` + `rustfmt --check` / `rustfmt`. |
| `make codegen` | Regenerate the golden IPC fixtures **and** the OpenAPI schema + web TS types (all from Rust). |
| `make codegen-check` | Fail if any of those drift from the Rust source. |
| `make audit` | CVE scan (`cargo audit` + `npm audit`). |
| `make licenses` | Fail on any copyleft dependency (`cargo deny`, policy in `rust/deny.toml`). |
| `make ci` | The full gate: lint (+ Tauri shell) + tests + codegen drift + version check + audit + licenses + web CI. Must stay green. |
| `make web-ci` | The web gate: `npm ci` + `tsc` + ESLint + `vite build`. |
| `make web-build` / `make web-typecheck` | Build the UI bundle (`web/dist`) / type-check it. |
| `make rust-serve` (`serve`) | Run the core (`SYNTHETIC=1` for no-permission plumbing). |
| `make dmg` | Build the unsigned/ad-hoc `.dmg` (see [packaging.md](packaging.md)). |

## Running

### `make rust-serve` — the API server

```sh
make rust-serve                       # auto-picks a free port; prints URL + per-session token
RUST_PORT=8137 make rust-serve
SYNTHETIC=1 make rust-serve           # drive the pipeline with generated audio (no mic/TCC)
```

Binds `127.0.0.1` and prints `open: http://127.0.0.1:<port>/?token=<token>` (see [api.md](api.md)).
This is what the web UI loads. `SYNTHETIC=1` runs the helper's tone source, so the whole
capture -> IPC -> sidecar -> DB -> transcript wiring runs without a mic or permission prompts. For a
real capture run, `make swift-build` first so the helper + sidecars exist.

### Web UI (`web/`)

Vite + React + TypeScript (strict). The core serves the built bundle at `/` with the session token
injected, so the production flow is build-then-serve:

```sh
cd web && npm ci          # install pinned deps (one time)
npm run build             # -> web/dist (served by the core)
```

For frontend development with hot reload, run the core on a fixed port and Vite in front of it (Vite
proxies `/api` + `/ws` to the core; see `web/vite.config.ts`):

```sh
RUST_PORT=8137 make rust-serve   # terminal 1
cd web && npm run dev                        # terminal 2 -> http://localhost:5173/?token=<token>
```

API TypeScript types are generated from the core's OpenAPI schema — never hand-edited: `make codegen`
runs `hearsay-core --dump-openapi` (-> `web/openapi.json`) then `openapi-typescript`
(-> `web/src/api/schema.ts`), and also regenerates the golden IPC fixtures from `hearsay-ipc`.
`make codegen-check` (and CI) fail if any of those drift. The WebSocket event types
(`TranscriptEvent`, `StatusEvent`, `ResyncEvent`) are modeled in the OpenAPI schema too, so
`web/src/api/ws.ts` aliases the generated `schema.ts` types rather than hand-maintaining them.

## Models

**ASR + diarization models** live in the Swift sidecars (FluidAudio on the ANE): Parakeet TDT for
ASR (`hearsay-live` / `hearsay-me`), pyannote community-1 as CoreML for the offline diarizer
(`hearsay-diarize`). Their CoreML models are ungated and auto-download + compile on first use — no
fetch step, no HF token.

**The offline refine** re-transcribes diarized turns with whisper (`hearsay-inference`), which needs
a GGML model. Download `ggml-large-v3-turbo.bin` into `outputs/models/` (the default
`HEARSAY_REFINE_MODEL` path); without it, auto-refine and `POST /api/meetings/{id}/rediarize` report
the sidecar/model as unavailable rather than failing the meeting. Packaging bundles this model into
the `.app` (see [packaging.md](packaging.md)).

**Notes (optional local LLM).** When the `notes` feature is built in (`make rust-serve` and `make
dmg` build it) and enabled (`HEARSAY_NOTES`, default off), stopping a meeting generates a summary +
action items from the finalized transcript with a local GGUF instruct model (llama.cpp via
`llama-cpp-2`). The model is chosen in **Settings > Models**, which lists a small catalog and
downloads the pick into `HEARSAY_MODELS_DIR` (`outputs/models` by default) with a SHA-256 check.
`HEARSAY_NOTES_MODEL` sets the active model path and `HEARSAY_NOTES_PROMPT` the template (its
`{transcript}` placeholder is filled at generation). Notes are best-effort — a missing model or a
generation error never fails the meeting.

The live Them stream is labeled Speaker 1..N by `hearsay-live`; the refine re-diarizes the whole Them
track for better accuracy and recognizes returning people by voiceprint. It runs on demand via the
"Refine speakers" button, and optionally at stop when `HEARSAY_AUTO_REFINE` is on (default off, so
back-to-back meetings are not slowed by the previous meeting's refine). Rename a
speaker in the UI (or `PUT /api/meetings/{id}/speakers/{cluster_id}`) to bind a name that persists,
carries across a re-diarize, and is suggested next meeting.

## Configuration

All config is resolved from the environment with loopback-safe defaults (`rust/crates/hearsay-core/src/config.rs`).
Common overrides:

| Setting | Env | Default |
|---|---|---|
| Database URL | `DATABASE_URL` | `sqlite://<repo>/outputs/db/hearsay.db` |
| Output dir | `HEARSAY_OUTPUT_DIR` | `<repo>/outputs/recordings` |
| Web bundle dir | `HEARSAY_WEB_DIR` | `<repo>/web/dist` |
| Helper path | `HEARSAY_HELPER_PATH` | `helper/.build/arm64-apple-macosx/debug/hearsay-helper` |
| Refine model | `HEARSAY_REFINE_MODEL` | `outputs/models/ggml-large-v3-turbo.bin` |
| Record meeting audio (`audio.wav`) | `HEARSAY_RECORD` | `true` |
| Auto-refine at finalize | `HEARSAY_AUTO_REFINE` | `false` |
| Recognition threshold | `HEARSAY_RECOGNITION_THRESHOLD` | `0.6` |
| Notes (local-LLM summary) | `HEARSAY_NOTES` | `false` |
| Notes model (GGUF) | `HEARSAY_NOTES_MODEL` | (unset until one is downloaded) |
| Notes prompt template | `HEARSAY_NOTES_PROMPT` | built-in template |
| Models download dir | `HEARSAY_MODELS_DIR` | `<repo>/outputs/models` |
| Environment | `ENVIRONMENT` | `development` |

`HEARSAY_RECORD` / `HEARSAY_AUTO_REFINE` / `HEARSAY_RECOGNITION_THRESHOLD` are the defaults for the
editable Settings sections; a stored preference overrides them (see [settings-panels.md](settings-panels.md)).

**Audio recording + playback.** When recording is on (default), each meeting records one
timeline-accurate **stereo** `audio.wav` (Me = left channel, Them = right). This single file serves
both playback — the UI plays it with the transcript highlighting in sync (click a line to seek) —
and the refine, which reads its Them channel. Privacy tradeoff: it retains the full raw audio; turn
it off (Settings, or `HEARSAY_RECORD=false`) to opt out (which also disables the refine — no
recording to re-diarize; delete-meeting removes the folder). Served by `GET /api/meetings/{id}/audio`.

When run from source, all runtime data (recordings, the SQLite DB, downloaded models) lives under the
repo's `outputs/` (gitignored). Override any path with the env vars above.

## Testing

```sh
make rust-test            # cargo test (unit + router integration tests)
make test                 # Swift cross-language self-test + cargo test
```

- Integration tests exercise the assembled axum router with `tower::ServiceExt::oneshot` against an
  in-memory SQLite DB, with the capture routes on `DisabledEngine` (503 / clean close) — no helper.
- Pure logic (`order_speakers`, `assign_segment_speaker`, voiceprint matching) is unit-tested
  directly in `hearsay-attribution`.
- The pipeline is tested end to end with scripted fakes in `hearsay-orchestrator` (fake audio source
  + stubbed transcribers); the refine uses a stub diarizer (no ML deps).
- `hearsay-ipc`'s `golden_fixtures` test and the Swift `hearsay-helper selftest` both validate the
  codec against `shared/fixtures/frames.jsonl`.

## Troubleshooting

- **macOS permission prompts on first capture.** The first `start_capture` blocks while macOS shows
  the Microphone / System Audio Recording prompts — click Allow. Grants persist for the ad-hoc-signed
  helper until the next `swift build` changes its code signature.
- **First transcription is slow.** The FluidAudio CoreML models (Parakeet, the diarizer) download and
  compile on first use, and each sidecar takes ~10 s to load Parakeet at startup. Warm utterances are
  fast.
- **"helper binary not found" / a sidecar's live transcription is off.** Build them all:
  `make swift-build`. The default capture-helper path is
  `helper/.build/arm64-apple-macosx/debug/hearsay-helper` (override via `HEARSAY_HELPER_PATH`); the
  sidecars are located as its siblings.
- **Refine / "Refine speakers" reports the model missing.** Download `ggml-large-v3-turbo.bin` into
  `outputs/models/` (or point `HEARSAY_REFINE_MODEL` at it).
- **Speakers over- or under-merge.** The live labels are approximate; run the refine ("Refine
  speakers") for a more accurate whole-track re-diarization. Manual renames are carried across it.
