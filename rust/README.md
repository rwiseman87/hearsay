# Hearsay Rust workspace

Cross-platform (macOS + Windows) foundation for Hearsay. Target architecture:
[`../docs/architecture-cross-platform.md`](../docs/architecture-cross-platform.md).

**Status: scaffold.** Crates are stubs (no logic, no external dependencies yet). The layout below is
the agreed decomposition; each crate is filled in incrementally, porting the corresponding Python
module. Dependencies are added and **pinned to verified latest stable versions** at implementation
time (never guessed here).

## Crate map

| Crate | Responsibility | Ports / replaces |
|---|---|---|
| `hearsay-ipc` | Binary media-frame codec (28-byte LE header) + NDJSON control protocol; byte-for-byte with `shared/protocol/ipc.md`, validated against `shared/fixtures/frames.jsonl` | `src/hearsay/helper/protocol.py`, Swift `HearsayIPC.FrameCodec` |
| `hearsay-db` | Persistence: SQLite via SQLx/SeaORM (WAL + busy_timeout), UUID PKs, `created_at`/`updated_at`, forward-only migrations | `src/hearsay/models/`, `src/hearsay/db/` |
| `hearsay-attribution` | Speaker attribution pure logic: cluster->name weighted-majority vote, cross-meeting voiceprint cosine matching, manual-label locks; unit-tested in isolation | `src/hearsay/services/speakers.py`, diarization mapping |
| `hearsay-orchestrator` | Spawns + supervises the capture and inference sidecars; routes 16 kHz PCM (Me/Them); owns the live transcription pipeline state machine | `src/hearsay/helper/supervisor.py`, `src/hearsay/transcript/` |
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
