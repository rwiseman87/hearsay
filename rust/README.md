# Hearsay Rust workspace

The Rust core of Hearsay and its supporting crates. Cross-platform foundation (macOS today; Windows
is the remaining work). Target architecture:
[`../docs/architecture-cross-platform.md`](../docs/architecture-cross-platform.md).

The macOS live path runs end-to-end through `hearsay-core`: capture → streaming captions →
diarization → offline refine → optional local-LLM notes, all behind the loopback HTTP/WS API the
React UI consumes. The whole workspace is gated by `make ci` (`clippy -D warnings` + `rustfmt` +
`cargo test` + codegen-drift + `cargo audit` + `cargo deny`).

## Run it (macOS)

```sh
make swift-build                             # build hearsay-helper + the FluidAudio sidecars (once)
(cd web && npm ci && npm run build)          # build web/dist (once)
make rust-serve                              # runs hearsay-core (--features metal,notes); prints a ?token= URL
```

`make rust-serve` binds `127.0.0.1` on `RUST_PORT` (default 8799) and prints
`open: http://127.0.0.1:<port>/?token=<token>`. Start a meeting and Me/Them captions stream in live
with speaker diarization; **Refine speakers** after stop runs the offline re-diarize + higher-accuracy
re-transcription. `SYNTHETIC=1 make rust-serve` drives the pipeline with generated audio (no
mic/TCC). `HEARSAY_HELPER_PATH` / `HEARSAY_REFINE_MODEL` / `HEARSAY_NOTES_MODEL` override the helper +
models.

## Crate map

Nine crates; edges are `path` dependencies.

| Crate | Responsibility |
|---|---|
| `hearsay-ipc` | Binary media-frame codec (28-byte LE header) + NDJSON control protocol; the byte-for-byte source of truth for `shared/protocol/ipc.md`, validated against `shared/fixtures/frames.jsonl`. No internal deps. |
| `hearsay-attribution` | Speaker attribution pure logic: `order_speakers`, `assign_segment_speaker`, cross-meeting voiceprint cosine matching. Zero deps; unit-tested in isolation. |
| `hearsay-db` | Persistence: SQLite via SQLx (WAL + `busy_timeout` + foreign keys), UUID PKs, forward-only numbered migrations; the attribution *policy* (vote + recognition), FTS transcript search, folders, and notes/models queries. |
| `hearsay-engine` | The neutral `LiveEngine` trait seam (meeting lifecycle + live-transcript subscribe + rediarize) and the `DisabledEngine` placeholder the whole API test suite runs against. Exists to break the core↔orchestrator cycle. |
| `hearsay-orchestrator` | Implements `LiveEngine`: creates the meeting row + folder, drives an `AudioSource`, routes each stream's 16 kHz PCM to its `Transcriber`, records the stereo `audio.wav`, persists + broadcasts partials/finals, and runs the refine + notes off the op-lock at stop. Ships the scripted `testing` fakes. |
| `hearsay-backends` | Per-OS backend wiring behind the engine seam: `MacBackend` (warm sidecar pool) + `MacRefiner`, `build_engine`, and the rediarize/notes glue. The `#[cfg(feature="sherpa")]` `SherpaTranscriber` is the future Windows live path. |
| `hearsay-capture` | Audio capture behind the `AudioSource` trait. macOS: `SwiftHelperSource` drives the Swift `hearsay-helper` (Core Audio tap + mic) over the `hearsay-ipc` sockets. Plus the TCC permissions probe. |
| `hearsay-inference` | Local ML: the offline whisper ASR (`whisper-rs`, GGML; CPU + `metal`/`vulkan`/`cuda`) + the refine (`hearsay-diarize` sidecar + whisper re-transcribe); the optional `notes` local-LLM summary (`llama-cpp-2`); the `#[cfg(feature="sherpa")]` sherpa-onnx streaming/diarize (Windows path). |
| `hearsay-core` | The application binary: axum HTTP + WebSocket API (loopback + per-session token), serves the React UI, and the composition root. Depends on `hearsay-engine` + `hearsay-backends` + `hearsay-db` + `hearsay-ipc` (the concrete backends are hidden behind the seam). |

The **Tauri shell** lives at `../web/src-tauri/` (`tauri.conf.json` + `src/main.rs`): it bundles +
spawns `hearsay-core` and the Swift sidecars. It is a separate crate (not a workspace member) but is
linted + CVE/license-gated in `make ci`. Build the app with `make dmg` (see
[`../docs/packaging.md`](../docs/packaging.md)).

## Cargo features

- `metal` / `vulkan` / `cuda` — per-OS GPU acceleration for the whisper refine (and, with `notes`,
  the notes LLM). The macOS bundle builds `metal`; the default is portable CPU.
- `notes` — the optional local-LLM summary + action-items step (`llama-cpp-2`). Built into
  `make rust-serve` / `make dmg`; off at runtime unless `HEARSAY_NOTES` or the Settings toggle is on.
- `sherpa` — the cross-platform (Windows) sherpa-onnx live/diarize path. Off by default so the macOS
  bundle never compiles or links onnxruntime; the only consumers today are `#[ignore]`d tests.

## Conventions

Dependencies are pinned via `Cargo.lock` (add one only for clear value). `ApiError` never leaks SQL
to a client; list endpoints return `{ total, page, page_size, items }`. Migrations are forward-only
numbered `.sql` in `crates/hearsay-db/migrations/`. Run the gate with `make ci` (or `make rust-test`
/ `make rust-lint`; source `~/.cargo/env` first if `cargo` is not on PATH).
