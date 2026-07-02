# Hearsay Rust workspace

Cross-platform (macOS + Windows) foundation for Hearsay. Target architecture:
[`../docs/architecture-cross-platform.md`](../docs/architecture-cross-platform.md).

**Status (2026-07-02): 6 of 8 crates implemented + tested; 2 stubs remain.**

- **Implemented + tested** (`cargo test` + `clippy -D warnings` + `rustfmt`, gated by `make ci`):
  `hearsay-ipc`, `hearsay-attribution`, `hearsay-db`, `hearsay-engine`, `hearsay-core`,
  `hearsay-orchestrator`.
- **Stubs** (responsibilities below, ported incrementally): `hearsay-capture`, `hearsay-inference`.

`hearsay-core` runs the full self-contained API surface — meetings/segments/speakers/identities
queries, pure-DB writes (rename, delete), audio file serving with Range, static UI + token
injection + CSP, loopback Host/Origin hardening, per-session bearer token, and a utoipa OpenAPI
document (`GET /openapi.json` / `--dump-openapi`). The meeting *lifecycle* (start/stop) and the live
transcript WebSocket sit behind the `LiveEngine` trait seam (in the neutral `hearsay-engine` crate,
so the API consumes it and the orchestrator implements it without a dependency cycle);
`hearsay-orchestrator` implements it (the built-in `DisabledEngine` is the fallback until the
orchestrator is wired into the binary).

`hearsay-orchestrator` is the `LiveEngine` implementation: it creates the meeting row + folder,
drives an `AudioSource` (capture), routes each stream's PCM to its `Transcriber`, and persists +
broadcasts the partial/final segments the transcribers emit (Them binds `Speaker N` clusters). The
two external backends are behind traits (`AudioSource` from `hearsay-capture`; `Transcriber` from
`hearsay-inference`, with a real `tokio::process` `ProcessTranscriber` faithful to the Python
sidecar stdio), so the whole lifecycle is tested with scripted fakes over in-memory SQLite. It also
ships a `WavFileSource` (a file-backed `AudioSource`) so the full pipeline can run end-to-end from a
recorded `audio.wav` with no hardware — the capstone test drives a WAV through two real
`ProcessTranscriber` sidecars (a `mock_sidecar` fixture) into SQLite and out to a re-encoded
`audio.wav` + a rendered `transcript.md`. At stop it writes the meeting folder: a timeline-accurate
stereo `audio.wav` (Me=L / Them=R, normalized for playback), a `transcript.md` (grouped
`### HH:MM:SS — Speaker` turns), and a `meeting.json`. Only the offline refine at stop is deferred
(gated on `hearsay-inference`). Finals persist to the DB — the API's source of truth — today.

Dependencies are pinned to verified latest stable versions via `cargo add` at implementation time
(never guessed here). Run the gate with `make rust-test` / `make rust-lint` (source `~/.cargo/env`
first if `cargo` is not on PATH).

## Crate map

| Crate | Responsibility | Ports / replaces |
|---|---|---|
| `hearsay-ipc` | Binary media-frame codec (28-byte LE header) + NDJSON control protocol; byte-for-byte with `shared/protocol/ipc.md`, validated against `shared/fixtures/frames.jsonl` | `src/hearsay/helper/protocol.py`, Swift `HearsayIPC.FrameCodec` |
| `hearsay-db` | Persistence: SQLite via SQLx/SeaORM (WAL + busy_timeout), UUID PKs, `created_at`/`updated_at`, forward-only migrations | `src/hearsay/models/`, `src/hearsay/db/` |
| `hearsay-attribution` | Speaker attribution pure logic: cluster->name weighted-majority vote, cross-meeting voiceprint cosine matching, manual-label locks; unit-tested in isolation | `src/hearsay/services/speakers.py`, diarization mapping |
| `hearsay-engine` | The neutral `LiveEngine` trait seam (meeting lifecycle + live-transcript subscribe) + the `DisabledEngine` stub; consumed by `hearsay-core`, implemented by `hearsay-orchestrator` (breaks the would-be cycle) | The Python `create_app` `SessionManager` injection point |
| `hearsay-orchestrator` | Implements `LiveEngine`: spawns/supervises the capture + inference sidecars, routes 16 kHz PCM (Me/Them), owns the live transcription pipeline state machine (partials/finals, offline refine at stop) | `src/hearsay/helper/supervisor.py`, `src/hearsay/transcript/` |
| `hearsay-capture` | Cross-platform audio capture behind one trait. `cfg(windows)`: WASAPI loopback (Them) + mic (Me) via cpal. `cfg(macos)`: Core Audio process tap (Swift helper fallback) / cpal. Resample to 16 kHz mono; monotonic `host_ts` | Swift `hearsay-helper` capture |
| `hearsay-core` | Application binary: axum HTTP + WebSocket API (loopback + session token), wires db + orchestrator + attribution, serves the React UI (or runs under Tauri) | `src/hearsay/api/`, app entrypoint |
| `hearsay-inference` | Local-only inference sidecar: Silero VAD (ONNX) + pure-Rust Segmenter (partial/final) + whisper.cpp ASR (Metal/Vulkan/CUDA/CPU) + offline diarization (sherpa-onnx / pyannote ONNX); tiered model selection by detected hardware. Speaks the `hearsay-ipc` contract | Replaces the FluidAudio Swift sidecars on the unified path |

The **Tauri shell** (`hearsay-app`, with `src-tauri/` + `tauri.conf.json`) is added via `cargo tauri
init` when the desktop shell work starts; it hosts the React UI in the system webview and bundles the
core + sidecars. Not scaffolded here to avoid pinning an unverified Tauri config schema.

## Intended dependencies (pinned at implementation time)

- Core: `tokio`, `axum`, `tower` / `tower-http`, `serde`, `validator`/`garde`, `tracing` +
  `tracing-subscriber`, `utoipa`.
- DB: `sqlx` or `sea-orm` (+ its migrations), `uuid`, `time`/`chrono`.
- IPC: `bytes` (+ `nom` if needed).
- Capture: `cpal`.
- Inference: a `whisper.cpp` binding (e.g. `whisper-rs`), `ort` (ONNX Runtime) for Silero + diarization.

## Build

```sh
cargo build            # from rust/  (scaffold compiles with no external deps)
cargo test
```
