# Development

How to set up, build, run, test, and troubleshoot the project from source.

## Prerequisites

- A Rust toolchain ([rustup](https://rustup.rs/)); builds the core and all crates.
- Swift; the Command Line Tools are enough to build the helper and sidecars
  (`xcode-select --install`). Full Xcode is only needed for `.app`/`.dmg` packaging.
- Node 20.19+ (enforced by `web/package.json` `engines`); builds the React UI.
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
| `make swift-build` | Build the capture helper and the FluidAudio/ANE sidecars, one invocation each. |
| `make rust-build` / `make rust-test` | Build / test the Rust workspace. |
| `make test` | Build the helper, run the Swift cross-language self-test, then `cargo test`. |
| `make lint` / `make fmt` | clippy with warnings denied plus `rustfmt --check` (workspace and Tauri shell) / format. |
| `make codegen` | Regenerate the golden IPC fixtures and the OpenAPI schema plus web TS types, all from Rust. |
| `make codegen-check` | Fail if any generated artifact drifts from the Rust source. |
| `make audit` | CVE scan: `cargo audit` on both Rust trees plus `npm audit`. |
| `make licenses` | Fail on any copyleft dependency (`cargo deny`; policy in `rust/deny.toml`). |
| `make version` / `make set-version VERSION=x.y.z` | Print the app version / set it everywhere and regenerate codegen. |
| `make version-check` | Fail if the app version drifts across the five files that carry it. |
| `make version-check-tag` | Fail unless the release tag matches the app version (`TAG=`, else `GITHUB_REF_NAME`). |
| `make ci` | The full gate: lint, tests, codegen drift, version check, audit, licenses, web CI. Must stay green. |
| `make web-ci` | The web gate: `npm ci`, `tsc`, ESLint, vitest (unit/component tests), `vite build`. |
| `make rust-serve` (alias `serve`) | Build the `hearsay-notes` sidecar (`metal`) and run the core (`metal,aec`); the core spawns the sidecar for notes. |
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
fetch step, no Hugging Face token. In the packaged app the same download is driven up front by the
`hearsay-models` sidecar behind a first-run setup screen, so it happens with a progress bar rather
than mid-meeting (see [packaging.md](packaging.md)).

**The offline refine** re-transcribes diarized turns with whisper (`hearsay-inference`), which
needs a GGML model. `make fetch-refine-model` downloads `ggml-large-v3-turbo.bin` into
`outputs/models/` (the default `HEARSAY_REFINE_MODEL` path). Without it, auto-refine and
`POST /api/meetings/{id}/rediarize`
report the model as unavailable rather than failing the meeting. The installed app downloads its own
copy into app-data instead; nothing is bundled.

**Notes (optional local LLM).** When enabled (`HEARSAY_NOTES`, default off), stopping a meeting
generates Markdown notes from the finalized transcript with a local GGUF instruct model (llama.cpp)
run out-of-process in the `hearsay-notes` sidecar (see [architecture.md](architecture.md) for why it
is a separate binary). The model's reply is stored and rendered verbatim; the user-editable prompt
template dictates the format. `make rust-serve` and `make dmg` build + bundle the sidecar. Choose
the model in Settings > Models, which lists a small catalog
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

Every environment variable, its default, and the runtime settings overlay are in
[configuration.md](configuration.md). `Settings` in `rust/crates/hearsay-core/src/config.rs` is the
compiler-checked source that page mirrors.

Two things worth knowing while working from source:

- All runtime data — recordings, the SQLite database, downloaded models — lives under the repo's
  gitignored `outputs/` directory unless you override the paths.
- The dev output dir (`outputs/recordings`) is swept by the audio-archival pass like any other, so an
  old corpus recording there may have become `audio.flac`. Everything that reads a recording accepts
  either form.

### Audio recovery tools

Two tools live alongside the FLAC codec, both operating on a recordings root and neither touching a
sample:

```sh
# Decode an archived library back to audio.wav (add --delete-flac to reclaim the space).
cargo run --release -p hearsay-audio --example restore -- <recordings-dir>

# Rewrite the STREAMINFO frame-size range in files written before it was populated. Such files
# decode correctly but will not play in WKWebView or QuickTime; see docs/architecture.md.
cargo run --release -p hearsay-audio --example repair_header -- <recordings-dir>
```

## Windows

The Windows build targets `x86_64-pc-windows-msvc` only (no ARM). There is no Swift on Windows: capture is
in-process WASAPI and the live/refine models are the sherpa-onnx set.

Prerequisites on the Windows machine:

- Visual Studio 2022 Build Tools with the "Desktop development with C++" workload (MSVC + the
  Windows SDK).
- A Rust toolchain ([rustup](https://rustup.rs/); the default host triple is the MSVC one).
- [CMake](https://cmake.org) (the whisper-rs / llama-cpp-2 native builds).
- Node 20.19+.
- LLVM (`winget install -e --id LLVM.LLVM`) — llama-cpp-2 (the `hearsay-notes` sidecar) and `aec` run
  bindgen, which loads `libclang.dll` at build time.
- The [Vulkan SDK](https://vulkan.lunarg.com/sdk/home#windows) **and** Windows long-path support —
  the installer build enables the `vulkan` feature by default (GPU whisper refine + notes on any
  vendor's GPU; the live sherpa ASR is unaffected, as onnxruntime has no Vulkan provider). ggml
  builds its Vulkan shader generator as a nested cmake sub-project, and MSBuild's `.tlog` paths
  under it exceed `MAX_PATH` (`error MSB3491`) regardless of how short `CARGO_TARGET_DIR` is, so
  long paths are required. Enable them from an elevated PowerShell, then reboot:
  `Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -Value 1 -Type DWord`
  Pass `-NoVulkan` to `scripts\build-windows.ps1` to build CPU-only without either prerequisite.

Build and run from source (PowerShell; `scripts\build-windows.ps1` fetches the models on first
run, or fetch them from any host with `make fetch-sherpa-models`):

```powershell
cargo build --manifest-path rust\Cargo.toml --workspace --features sherpa
cargo run --manifest-path rust\Cargo.toml -p hearsay-core --features sherpa
cargo run --manifest-path rust\Cargo.toml -p hearsay-core --features sherpa -- --synthetic
```

(The notes LLM is not a core feature — it builds as the separate `hearsay-notes` sidecar; there is no
`notes` cargo feature.)

Everything links against the default dynamic CRT. `hearsay-inference` takes sherpa-onnx's `shared`
feature so onnxruntime + sherpa arrive as DLLs: the crate's default `static` libs are prebuilt
against the *static* CRT, which would force `-C target-feature=+crt-static` on the whole binary,
and whisper.cpp pins CMP0091 OLD so its cmake appends `/MD` after cmake-rs's `/MT` — a mismatch no
toolchain file can fix, because the platform defaults are set after a toolchain file runs.
`sherpa-onnx-sys` copies `sherpa-onnx-c-api.dll` + `onnxruntime.dll` next to the built binary; the
installer stages them alongside the sidecar.

`--synthetic` uses the built-in tone source, so the whole pipeline runs without a microphone or
system audio. The installer build is `scripts\build-windows.ps1` (see
[packaging.md](packaging.md)).

## Testing

The suite runs on demand from the Makefile -- no hosted CI, no timers, no git hooks. `make ci` is the
fast deterministic gate; the model/hardware probes, coverage, and the aggregate run are separate
targets. The full test-suite reference -- every target and what it tests, and the suite by layer --
is [testing.md](testing.md).

```sh
make ci         # deterministic gate: lint, tests, codegen drift, versions, supply-chain
make test       # Swift cross-language self-test + cargo test
make web-test   # web unit/component tests (vitest, jsdom); also folded into web-ci
make tauri-test # cargo test on the Tauri shell (web/src-tauri)
make probes     # the #[ignore]d model/hardware tests (needs the models + ANE/GPU)
make coverage   # cargo-llvm-cov + vitest v8 -> outputs/coverage/ (report-only)
make e2e        # browser E2E (Playwright/Chromium) vs the scripted core + vite
make test-all   # make ci + make probes + make e2e (run everything)
```

One-time setup for `make e2e`: `cd web && npm install && npx playwright install chromium`. Windows
has no `make`, so the same set is mirrored in `scripts\test-windows.ps1`
(`-Target ci|web|tauri|probes|coverage|e2e|all`).

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
- **The refine reports the model missing.** Run `make fetch-refine-model`, or point
  `HEARSAY_REFINE_MODEL` at an existing copy.
- **Speakers over- or under-merge.** The live labels are approximate; run "Refine speakers" for a
  more accurate whole-track re-diarization. Manual renames are carried across it.
