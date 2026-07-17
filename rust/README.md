# Hearsay Rust workspace

The Rust core of Hearsay and its supporting crates: the cross-platform foundation (macOS;
Windows planned). Architecture, crate diagrams, and trait seams:
[`../docs/architecture.md`](../docs/architecture.md).

The macOS live path runs end to end through `hearsay-core`: capture, streaming captions,
diarization, the offline refine, and optional local-LLM notes, all behind the loopback HTTP and
WebSocket API the React UI consumes. The whole workspace is gated by `make ci` (clippy with
warnings denied, rustfmt, `cargo test`, codegen drift, `cargo audit`, `cargo deny`).

## Run it (macOS)

```sh
make swift-build                      # build hearsay-helper + the FluidAudio sidecars (once)
(cd web && npm ci && npm run build)   # build web/dist (once)
make rust-serve                       # runs hearsay-core (--features metal,notes); prints a ?token= URL
```

`make rust-serve` binds `127.0.0.1` on `RUST_PORT` (default 8799) and prints
`open: http://127.0.0.1:<port>/?token=<token>`. Start a meeting and Me/Them captions stream in
live with speaker diarization; "Refine speakers" after stop runs the offline re-diarize and
higher-accuracy re-transcription. `SYNTHETIC=1 make rust-serve` drives the pipeline with generated
audio (no microphone, no TCC prompts). `HEARSAY_HELPER_PATH`, `HEARSAY_REFINE_MODEL`, and
`HEARSAY_NOTES_MODEL` override the helper and model paths.

## Crate map

Nine crates; edges are `path` dependencies (diagrammed in
[`../docs/architecture.md`](../docs/architecture.md)).

| Crate | Responsibility |
|---|---|
| `hearsay-ipc` | Binary media-frame codec (28-byte little-endian header) plus the NDJSON control protocol; the byte-for-byte source of truth for `shared/protocol/ipc.md`, validated against the golden fixtures in `shared/fixtures/`. No internal dependencies. |
| `hearsay-attribution` | Pure speaker-attribution logic: speaker ordering, segment-speaker assignment, cross-meeting voiceprint cosine matching. No dependencies; unit-tested in isolation. |
| `hearsay-db` | Persistence: SQLite via SQLx (WAL, `busy_timeout`, foreign keys), UUID primary keys, forward-only numbered migrations; the attribution policy (vote and recognition), FTS transcript search, folders, and the notes and models queries. |
| `hearsay-engine` | The neutral `LiveEngine` trait seam (meeting lifecycle, live-transcript subscribe, rediarize, notes) and the `DisabledEngine` placeholder the whole API test suite runs against. Exists to break the core/orchestrator cycle. |
| `hearsay-orchestrator` | Implements `LiveEngine`: creates the meeting row and folder, drives an `AudioSource`, routes each stream's 16 kHz PCM to its `Transcriber`, records the stereo `audio.wav`, persists and broadcasts partials and finals, and runs the refine and notes off the operation lock at stop. Ships the scripted `testing` fakes. |
| `hearsay-backends` | Per-OS backend wiring behind the engine seam: `MacBackend` (warm sidecar pool), `MacRefiner`, the notes summarizer, startup reconciliation, and `build_engine`. The feature-gated `SherpaTranscriber` is the future Windows live path. |
| `hearsay-capture` | Audio capture behind the `AudioSource` trait. On macOS, `SwiftHelperSource` drives the Swift `hearsay-helper` (Core Audio tap plus microphone) over the `hearsay-ipc` sockets. Also hosts the TCC permissions probe. |
| `hearsay-inference` | Local ML, all offline: the whisper ASR (`whisper-rs`, GGML; CPU plus `metal`/`vulkan`/`cuda` features) and the refine (the `hearsay-diarize` sidecar plus whisper re-transcription); the optional `notes` local-LLM summary (`llama-cpp-2`); the feature-gated sherpa-onnx streaming and diarize modules for the Windows path. |
| `hearsay-core` | The application binary: the axum HTTP and WebSocket API (loopback plus per-session token), the served React UI, and the composition root. Depends on `hearsay-engine`, `hearsay-backends`, `hearsay-db`, and `hearsay-ipc`; the concrete backends stay hidden behind the seam. |

The Tauri shell lives at `../web/src-tauri/` (`tauri.conf.json` plus `src/main.rs`): it bundles
and spawns `hearsay-core` and the Swift sidecars. It is a separate crate (not a workspace member)
but is linted and CVE/license-gated in `make ci`. Build the app with `make dmg` (see
[`../docs/packaging.md`](../docs/packaging.md)).

## Cargo features

- `metal` / `vulkan` / `cuda`: per-OS GPU acceleration for the whisper refine (and, with `notes`,
  the notes LLM). The macOS bundle builds `metal`; the default is portable CPU.
- `notes`: the optional local-LLM summary and action-items step (`llama-cpp-2`). Built into
  `make rust-serve` and `make dmg`; off at runtime unless `HEARSAY_NOTES` or the Settings toggle
  turns it on.
- `sherpa`: the cross-platform (Windows) sherpa-onnx live and diarize path. Off by default, so the
  macOS bundle never compiles or links onnxruntime; the only consumers are ignored tests.

## Conventions

Dependencies are pinned via `Cargo.lock`; add one only for clear value. `ApiError` never leaks SQL
to a client; list endpoints return `{ total, page, page_size, items }`. Migrations are
forward-only numbered `.sql` files in `crates/hearsay-db/migrations/`. Run the gate with `make ci`
(or `make rust-test` / `make rust-lint`; source `~/.cargo/env` first if `cargo` is not on PATH).
