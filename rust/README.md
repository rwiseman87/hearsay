# Hearsay Rust workspace

The Rust core of Hearsay and its supporting crates — the macOS foundation. Architecture, crate diagrams, and trait seams:
[`../docs/architecture.md`](../docs/architecture.md).

The live path runs end to end through `hearsay-core`: capture, streaming captions, diarization, the
offline refine, and optional local-LLM notes, all behind the loopback HTTP and WebSocket API the
React UI consumes. The whole workspace is gated by `make ci` (clippy with
warnings denied, rustfmt, `cargo test`, codegen drift, `cargo audit`, `cargo deny`).

## Run it (macOS)

```sh
make swift-build                      # build hearsay-helper + the FluidAudio sidecars (once)
(cd web && npm ci && npm run build)   # build web/dist (once)
make rust-serve                       # runs hearsay-core (--features aec,api-console); prints a ?token= URL
```

`make rust-serve` binds `127.0.0.1` on `RUST_PORT` (default 8799) and prints
`open: http://127.0.0.1:<port>/?token=<token>`. Start a meeting and Me/Them captions stream in
live with speaker diarization; "Refine speakers" after stop runs the offline re-diarize and
higher-accuracy re-transcription. `SYNTHETIC=1 make rust-serve` drives the pipeline with generated
audio (no microphone, no TCC prompts). `HEARSAY_HELPER_PATH` and `HEARSAY_NOTES_MODEL` override the helper and notes model paths.

## Crate map

The crates and their responsibilities are tabulated in
[`../docs/architecture.md`](../docs/architecture.md), which is canonical and carries the dependency
graph. Edges below are `path` dependencies (diagrammed there).

| Crate | Responsibility |
|---|---|
| `hearsay-ipc` | Binary media-frame codec (28-byte little-endian header) plus the NDJSON control protocol; the byte-for-byte source of truth for `shared/protocol/ipc.md`, validated against the golden fixtures in `shared/fixtures/`. No internal dependencies. |
| `hearsay-attribution` | Pure speaker-attribution logic: speaker ordering, segment-speaker assignment, cross-meeting voiceprint cosine matching. No dependencies; unit-tested in isolation. |
| `hearsay-audio` | Lossless FLAC archival of the recorded meeting WAV: a bounded-memory streaming encoder, a block-at-a-time decoder, and a byte-exact verifier that must pass before the original is deleted. Ships `restore` and `repair_header` examples for un-archiving a library and for repairing STREAMINFO. No first-party dependencies; unit-tested in isolation. |
| `hearsay-db` | Persistence: SQLite via SQLx (WAL, `busy_timeout`, foreign keys), UUID primary keys, forward-only numbered migrations; the attribution policy (vote and recognition), FTS transcript search, folders, and the notes and models queries. |
| `hearsay-engine` | The neutral `LiveEngine` trait seam (meeting lifecycle, live-transcript subscribe, rediarize, notes) and the `DisabledEngine` placeholder the whole API test suite runs against. Exists to break the core/orchestrator cycle. |
| `hearsay-orchestrator` | Implements `LiveEngine`: creates the meeting row and folder, drives an `AudioSource`, routes each stream's 16 kHz PCM to its `Transcriber`, records the stereo `audio.wav`, persists and broadcasts partials and finals, and runs the refine and notes off the operation lock at stop. Ships the scripted `testing` fakes. |
| `hearsay-backends` | Backend wiring behind the engine seam: `MacBackend` (warm sidecar pool), `MacRefiner`, the `SubprocessSummarizer` (spawns the `hearsay-notes` sidecar), startup reconciliation, and `build_engine`. |
| `hearsay-capture` | Audio capture behind the `AudioSource` trait. On macOS, `SwiftHelperSource` drives the Swift `hearsay-helper` (Core Audio tap plus microphone) over the `hearsay-ipc` sockets. Also hosts the TCC permissions probe. |
| `hearsay-inference` | The offline refine: drives the `hearsay-diarize` sidecar (community-1 diarization + Parakeet Ultra ASR), attributes each word to a diarizer turn, and guards transcript coverage. No ML runs in-process. |
| `hearsay-notes` | The local-LLM notes sidecar: a standalone binary that owns llama.cpp (`llama-cpp-2`), spawned by the core over stdio (JSON in, JSON out). |
| `hearsay-notes-prompt` | Dependency-free prompt construction + reply parsing for the notes step, shared by the core's config default and the sidecar (so the sidecar never pulls `hearsay-inference`). |
| `hearsay-eval` | Accuracy and latency evals, test-only: corpus and reference-transcript loading, the baseline gate, a driver that feeds the live sidecars over stdio and timestamps what they emit, and the `asr_accuracy` / `live_eval` tests behind `make wer-eval` / `make live-eval`. Scoring is pure `hearsay-attribution` (`word_errors`, `cpwer`, `percentiles`). |
| `hearsay-core` | The application binary: the axum HTTP and WebSocket API (loopback plus per-session token), the served React UI, and the composition root. Depends on `hearsay-engine`, `hearsay-backends`, `hearsay-db`, and `hearsay-ipc`; the concrete backends stay hidden behind the seam. |

The Tauri shell lives at `../web/src-tauri/` (`tauri.conf.json` plus `src/main.rs`): it bundles
and spawns `hearsay-core`, and bundles the Swift sidecars plus the `hearsay-notes` sidecar (the core
spawns that one). It is a separate crate (not a workspace member)
but is linted and CVE/license-gated in `make ci`. Build the app with `make dmg` (see
[`../docs/packaging.md`](../docs/packaging.md)).

## Cargo features

- `aec`: acoustic echo cancellation (SpeexDSP via `aec-rs`) on the live Me stream, using the Them
  tap as the far-end reference. `make rust-serve` and the release bundle build it (`--features
  aec`); the raw pre-AEC audio is what the recorder and offline refine read. See
  [`../docs/echo-cancellation.md`](../docs/echo-cancellation.md).
- `api-console`: the browsable Swagger UI at `/docs`, for working against the API by hand. Off by
  default — the vendored assets are embedded at compile time, so a runtime check alone would still
  ship them. `make rust-serve` enables it; the mount additionally requires
  `ENVIRONMENT=development`. See [`../docs/api.md`](../docs/api.md).

The notes LLM is not a core feature: it ships as the standalone `hearsay-notes` sidecar (built with
its own `metal` feature for GPU acceleration), so a llama.cpp crash cannot take down the core. `make
rust-serve` and `make dmg` build and bundle it; it is off at runtime unless `HEARSAY_NOTES` or the
Settings toggle turns it on.

## Conventions

Dependencies are pinned via `Cargo.lock`; add one only for clear value. `ApiError` never leaks SQL
to a client; list endpoints return `{ total, page, page_size, items }`. Migrations are
forward-only numbered `.sql` files in `crates/hearsay-db/migrations/`. Run the gate with `make ci`
(or `make rust-test` / `make rust-lint`; source `~/.cargo/env` first if `cargo` is not on PATH).
