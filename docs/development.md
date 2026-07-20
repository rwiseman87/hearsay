# Development

How to set up, build, run, test, and troubleshoot the project from source.

## Prerequisites

- A Rust toolchain ([rustup](https://rustup.rs/)); builds the core and all crates.
- Swift; the Command Line Tools are enough to build the helper and sidecars
  (`xcode-select --install`). Full Xcode is only needed for `.app`/`.dmg` packaging.
- Node 22; builds the React UI.
- Apple Silicon, macOS 14.4 or later (Core Audio process taps).

## Setup

```sh
make swift-build                      # build hearsay-helper + the FluidAudio/ANE sidecars
(cd web && npm ci && npm run build)   # build the UI bundle (web/dist), served by the core
```

There is no interpreter to install: the core is a single Rust binary. The Swift sidecars' CoreML
models download automatically on first use; the whisper refine model is a separate download (see
[Models](#models)).

## Make targets

The `Makefile` is the task runner.

| Target | What it does |
|---|---|
| `make swift-build` | Build the capture helper and the FluidAudio/ANE sidecars (explicit products). |
| `make rust-build` / `make rust-test` | Build / test the Rust workspace. |
| `make test` | Build the helper, run the Swift cross-language self-test, then `cargo test`. |
| `make lint` / `make fmt` | clippy with warnings denied plus `rustfmt --check` (workspace and Tauri shell) / format. |
| `make codegen` | Regenerate the golden IPC fixtures and the OpenAPI schema plus web TS types, all from Rust. |
| `make codegen-check` | Fail if any generated artifact drifts from the Rust source. |
| `make audit` | CVE scan: `cargo audit` on both Rust trees plus `npm audit`. |
| `make licenses` | Fail on any copyleft dependency (`cargo deny`; policy in `rust/deny.toml`). |
| `make version-check` | Fail if the app version drifts across the workspace, Tauri config, and package.json. |
| `make ci` | The full gate: lint, tests, codegen drift, version check, audit, licenses, web CI. Must stay green. |
| `make web-ci` | The web gate: `npm ci`, `tsc`, ESLint, `vite build`. |
| `make rust-serve` (alias `serve`) | Run the core with the `metal` and `notes` features. |
| `make dmg` | Build the unsigned, ad-hoc-signed `.dmg` (see [packaging.md](packaging.md)). |

## Running

### `make rust-serve` (the API server)

```sh
make rust-serve                # binds RUST_PORT (default 8799); prints the tokenized URL
RUST_PORT=8137 make rust-serve
SYNTHETIC=1 make rust-serve    # generated audio; no microphone, no TCC prompts
```

Binds `127.0.0.1` and prints `open: http://127.0.0.1:<port>/?token=<token>` (see
[api.md](api.md)). This is what the web UI loads. `SYNTHETIC=1` runs the helper's tone source, so
the whole capture, IPC, sidecar, database, and transcript wiring runs without a microphone or
permission prompts. For a real capture run, `make swift-build` first so the helper and sidecars
exist. Note that `rust-serve` points `DATABASE_URL` at its own database file
(`outputs/db/hearsay-rust.db`), separate from any other local database.

### Web UI (`web/`)

Vite, React, and TypeScript in strict mode. The core serves the built bundle at `/` with the
session token injected, so the production flow is build-then-serve:

```sh
cd web && npm ci    # install pinned deps (one time)
npm run build       # -> web/dist, served by the core
```

For frontend development with hot reload, run the core on a fixed port and Vite in front of it
(Vite proxies `/api` and `/ws` to the core; see `web/vite.config.ts`):

```sh
RUST_PORT=8137 make rust-serve    # terminal 1
cd web && npm run dev             # terminal 2 -> http://localhost:5173/?token=<token>
```

API TypeScript types are generated from the core's OpenAPI schema and never hand-edited:
`make codegen` runs `hearsay-core --dump-openapi` (writing `web/openapi.json`), then
`openapi-typescript` (writing `web/src/api/schema.ts`), and regenerates the golden IPC fixtures
from `hearsay-ipc`. `make codegen-check` (and CI) fail if any of those drift. The WebSocket event
types (`TranscriptEvent`, `StatusEvent`, `ResyncEvent`) are modeled in the OpenAPI schema too, so
`web/src/api/ws.ts` aliases the generated types rather than hand-maintaining them.

## Models

**ASR and diarization models** live in the Swift sidecars (FluidAudio on the ANE): Parakeet TDT
for ASR (`hearsay-live`, `hearsay-me`) and pyannote community-1 as CoreML for the offline diarizer
(`hearsay-diarize`). These are ungated and download plus compile automatically on first use; no
fetch step, no Hugging Face token.

**The offline refine** re-transcribes diarized turns with whisper (`hearsay-inference`), which
needs a GGML model. Download `ggml-large-v3-turbo.bin` into `outputs/models/` (the default
`HEARSAY_REFINE_MODEL` path). Without it, auto-refine and `POST /api/meetings/{id}/rediarize`
report the model as unavailable rather than failing the meeting. Packaging bundles this model into
the `.app` (see [packaging.md](packaging.md)).

**Notes (optional local LLM).** When the `notes` feature is built in (`make rust-serve` and
`make dmg` both build it) and enabled (`HEARSAY_NOTES`, default off), stopping a meeting generates
a summary and action items from the finalized transcript with a local GGUF instruct model
(llama.cpp via `llama-cpp-2`). Choose the model in Settings > Models, which lists a small catalog
and downloads the pick into `HEARSAY_MODELS_DIR` with a SHA-256 check. `HEARSAY_NOTES_MODEL` sets
the active model path and `HEARSAY_NOTES_PROMPT` the template (its `{transcript}` placeholder is
filled at generation). Notes are best-effort: a missing model or a generation error never fails
the meeting.

The live Them stream is labeled `Speaker 1..N` by `hearsay-live`; the refine re-diarizes the whole
Them track for better accuracy and recognizes returning people by voiceprint. It runs on demand
from the "Refine speakers" button, and at stop when `HEARSAY_AUTO_REFINE` is on (default off, so
back-to-back meetings are not slowed by the previous meeting's refine). Rename a speaker in the UI
(or with `PUT /api/meetings/{id}/speakers/{cluster_id}`) to bind a name that persists, survives a
re-diarize, and is suggested next meeting.

## Configuration

All configuration is resolved from the environment into one typed settings struct with
loopback-safe defaults (`rust/crates/hearsay-core/src/config.rs`). A malformed override (a boolean
typo, an unparseable number, an out-of-range threshold) is a warning in development and a startup
error otherwise, so a misconfigured deploy fails fast instead of silently using a default.

| Setting | Env | Default |
|---|---|---|
| Database URL | `DATABASE_URL` | `sqlite://./outputs/db/hearsay.db` |
| Output dir | `HEARSAY_OUTPUT_DIR` | `./outputs/recordings` |
| Web bundle dir | `HEARSAY_WEB_DIR` | `./web/dist` |
| Bind host / port | `HEARSAY_SERVER_HOST` / `HEARSAY_SERVER_PORT` | `127.0.0.1` / `0` (OS-assigned) |
| Helper path | `HEARSAY_HELPER_PATH` | `helper/.build/arm64-apple-macosx/debug/hearsay-helper` |
| Refine model | `HEARSAY_REFINE_MODEL` | `outputs/models/ggml-large-v3-turbo.bin` |
| Refine timeout (seconds) | `HEARSAY_REFINE_TIMEOUT_SECS` | `1800` |
| Record meeting audio (`audio.wav`) | `HEARSAY_RECORD` | `true` |
| Auto-refine at stop | `HEARSAY_AUTO_REFINE` | `false` |
| Recognition threshold | `HEARSAY_RECOGNITION_THRESHOLD` | `0.6` |
| Notes (local-LLM summary) | `HEARSAY_NOTES` | `false` |
| Notes model (GGUF) | `HEARSAY_NOTES_MODEL` | unset until one is downloaded |
| Notes prompt template | `HEARSAY_NOTES_PROMPT` | built-in template |
| Models download dir | `HEARSAY_MODELS_DIR` | `outputs/models` |
| Shell handshake file | `HEARSAY_HANDSHAKE_PATH` | unset (headless dev prints the URL instead) |
| Bundled FluidAudio models | `HEARSAY_FLUID_MODELS_DIR` | unset (FluidAudio downloads to its cache) |
| Sherpa models dir (Windows backend) | `HEARSAY_SHERPA_MODELS_DIR` | `outputs/models/sherpa` |
| Them loopback path (Windows) | `HEARSAY_WIN_LOOPBACK` | `device` (`device` \| `process`) |
| Environment | `ENVIRONMENT` | `development` |

`HEARSAY_RECORD`, `HEARSAY_AUTO_REFINE`, `HEARSAY_RECOGNITION_THRESHOLD`, and the notes settings
are the defaults for the editable Settings sections; a stored preference overrides them. The
handshake and FluidAudio paths are injected by the desktop shell and normally unset in
development. The sherpa and loopback settings apply only on Windows (see
[windows-port.md](windows-port.md)).

**Audio recording and playback.** When recording is on (the default), each meeting records one
timeline-accurate stereo `audio.wav` (Me on the left channel, Them on the right). This single file
serves both playback (the UI plays it with the transcript highlighted in sync; click a line to
seek) and the refine, which reads its Them channel. The tradeoff is that it retains the full raw
audio: turn it off in Settings or with `HEARSAY_RECORD=false` to opt out, which also disables the
refine since there is no recording to re-diarize. Deleting a meeting removes the folder. Served by
`GET /api/meetings/{id}/audio`.

When run from source, all runtime data (recordings, the SQLite database, downloaded models) lives
under the repo's `outputs/` directory, which is gitignored. Override any path with the variables
above.

## Windows

The Windows build targets `x86_64-pc-windows-msvc` only (no ARM); the port's plan and tracking
state live in [windows-port.md](windows-port.md). There is no Swift on Windows: capture is
in-process WASAPI and the live/refine models are the sherpa-onnx set.

Prerequisites on the Windows machine:

- Visual Studio 2022 Build Tools with the "Desktop development with C++" workload (MSVC + the
  Windows SDK).
- A Rust toolchain ([rustup](https://rustup.rs/); the default host triple is the MSVC one).
- [CMake](https://cmake.org) (the whisper-rs / llama-cpp-2 native builds).
- Node 22.
- Only for the `aec` feature: LLVM (libclang, for bindgen).
- Only for the `vulkan` feature: the Vulkan SDK.

Build and run from source (PowerShell; `scripts\build-windows.ps1` fetches the models on first
run, or fetch them from any host with `make fetch-sherpa-models`):

```powershell
cargo build --manifest-path rust\Cargo.toml --features sherpa,notes
cargo run --manifest-path rust\Cargo.toml -p hearsay-core --features sherpa,notes
cargo run --manifest-path rust\Cargo.toml -p hearsay-core --features sherpa,notes -- --synthetic
```

`--synthetic` uses the built-in tone source, so the whole pipeline runs without a microphone or
system audio. The installer build is `scripts\build-windows.ps1` (see
[packaging.md](packaging.md)).

## Testing

```sh
make rust-test    # cargo test (unit + router integration tests)
make test         # Swift cross-language self-test + cargo test
```

- Integration tests exercise the assembled axum router with `tower::ServiceExt::oneshot` against
  an in-memory SQLite database, with the capture routes on `DisabledEngine` (503 / clean close);
  no helper involved.
- Pure logic (speaker ordering, segment-speaker assignment, voiceprint matching) is unit-tested
  directly in `hearsay-attribution`.
- The pipeline is tested end to end with scripted fakes in `hearsay-orchestrator` (a fake audio
  source plus stubbed transcribers); the refine uses a stub diarizer. No ML dependencies.
- `hearsay-ipc`'s golden-fixture tests and the Swift `hearsay-helper selftest` both validate the
  codec against `shared/fixtures/`.

## Troubleshooting

- **macOS permission prompts on first capture.** The first `start_capture` blocks while macOS
  shows the Microphone and System Audio Recording prompts; click Allow. Grants persist for the
  ad-hoc-signed helper until the next `swift build` changes its code signature.
- **First transcription is slow.** The FluidAudio CoreML models download and compile on first use,
  and each sidecar takes about ten seconds to load Parakeet at startup. Warm utterances are fast.
- **"helper binary not found", or a sidecar's live transcription is missing.** Build them all with
  `make swift-build`. The default capture-helper path is
  `helper/.build/arm64-apple-macosx/debug/hearsay-helper` (override with `HEARSAY_HELPER_PATH`);
  the sidecars are located as its siblings.
- **The refine reports the model missing.** Download `ggml-large-v3-turbo.bin` into
  `outputs/models/`, or point `HEARSAY_REFINE_MODEL` at it.
- **Speakers over- or under-merge.** The live labels are approximate; run "Refine speakers" for a
  more accurate whole-track re-diarization. Manual renames are carried across it.
