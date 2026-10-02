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
models, including the refine's, download automatically on first use (see [Models](#models)).

## Make targets

The `Makefile` is the task runner.

| Target | What it does |
|---|---|
| `make swift-build` | Build the capture helper and the FluidAudio/ANE sidecars, one invocation each. |
| `make rust-build` / `make rust-test` | Build / test the Rust workspace. |
| `make test` | Build the helper, run the Swift cross-language self-test, then `cargo test`. |
| `make lint` / `make fmt` | clippy with warnings denied plus `rustfmt --check` / rustfmt, both on the workspace and the Tauri shell. |
| `make codegen` | Regenerate the golden IPC fixtures and the OpenAPI schema plus web TS types, all from Rust. |
| `make codegen-check` | Fail if any generated artifact drifts from the Rust source. |
| `make audit` | CVE scan: `cargo audit` on both Rust trees plus `npm audit`. |
| `make licenses` | Fail on any copyleft dependency (`cargo deny`; policy in `rust/deny.toml`). |
| `make version` / `make set-version VERSION=x.y.z` | Print the app version / set it everywhere and regenerate codegen. |
| `make version-check` | Fail if the app version drifts across the five files that carry it. |
| `make stamp-version VERSION=x.y.z` | Write the version into the five files without codegen; what the release build runs. |
| `make ci` | The full gate: lint, tests, codegen drift, version check, audit, licenses, web CI. Must stay green. |
| `make web-ci` | The web gate: `npm ci`, `tsc`, ESLint, vitest (unit/component tests), `vite build`. |
| `make rust-serve` | Build the `hearsay-notes` sidecar (`metal`) and run the core (`aec`); the core spawns the sidecar for notes. |
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

**ASR and diarization models** live in the Swift sidecars (FluidAudio on the ANE): Parakeet
for live ASR (`hearsay-live`, `hearsay-me`), and pyannote community-1 as CoreML plus Parakeet Ultra for
the offline refine (`hearsay-diarize`). These are ungated and download plus compile automatically on first use; no
fetch step, no Hugging Face token. In the packaged app the same download is driven up front by the
`hearsay-models` sidecar behind a first-run setup screen, so it happens with a progress bar rather
than mid-meeting (see [packaging.md](packaging.md)).

**The offline refine** runs one `hearsay-diarize <wav> --asr ultra` sidecar over the Them track: it
diarizes (pyannote community-1) and transcribes with Parakeet Ultra, and `hearsay-inference`
attributes each word to a speaker turn. The Ultra model is fetched by the same first-use download as
the other FluidAudio models (`make swift-build` builds the sidecar; no separate fetch step). Until
it is present, auto-refine and `POST /api/meetings/{id}/rediarize` report the model as unavailable
rather than failing the meeting.

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

## Testing

The suite runs from the Makefile -- no timers, no git hooks. `make ci` is the fast deterministic gate
and runs in GitHub Actions on every push and pull request; the model/hardware probes, coverage, and the
aggregate run are separate targets, on demand. The full test-suite reference -- every target and what it
tests, and the suite by layer -- is [testing.md](testing.md).

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

One-time setup for `make e2e`: `cd web && npm install && npx playwright install chromium`.

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
- **The refine reports the model missing.** Finish first-run setup so the FluidAudio
  models, including Parakeet Ultra, are downloaded.
- **Speakers over- or under-merge.** The live labels are approximate; run "Refine speakers" for a
  more accurate whole-track re-diarization. Manual renames are carried across it.
