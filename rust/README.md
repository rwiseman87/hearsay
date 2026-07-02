# Hearsay Rust workspace

Cross-platform (macOS + Windows) foundation for Hearsay. Target architecture:
[`../docs/architecture-cross-platform.md`](../docs/architecture-cross-platform.md).

**Status (2026-07-02): the macOS live path runs end-to-end through the Rust core.**

- **Implemented + tested** (`cargo test` + `clippy -D warnings` + `rustfmt`, gated by `make ci`):
  `hearsay-ipc`, `hearsay-attribution`, `hearsay-db`, `hearsay-engine`, `hearsay-orchestrator`,
  `hearsay-capture`, `hearsay-core` (binary wires the real `Orchestrator` + a macOS `Backend`).
- **`hearsay-capture` (macOS):** `SwiftHelperSource` reuses the proven Swift `hearsay-helper` (Core
  Audio tap + mic) over the `hearsay-ipc` sockets → `CaptureChunk`s. Verified against the real helper
  in `--synthetic` mode. The Rust `ProcessTranscriber` spawns the built `hearsay-live` / `hearsay-me`
  FluidAudio sidecars directly (identical stdio protocol), so **live streaming + diarization on the
  Mac reuse the working Swift stack** (the "FluidAudio as macOS tier" fork). Windows cpal capture is
  the other half, later.
- **`hearsay-inference` — offline ASR + refine done:** whisper.cpp via `whisper-rs` (GGML → timestamped
  segments; CPU + `metal`/`vulkan`/`cuda` features). Verified on the Mac (`jfk.wav` verbatim, ~50x RT
  CPU / ~25x RT Metal). The **offline refine** (`refine_them`) re-diarizes the Them track via the Swift
  `hearsay-diarize` sidecar + re-transcribes each turn with whisper; it's wired to `POST /rediarize` (the
  frontend "Refine speakers" button, validated on-device). A pure-Rust streaming `Transcriber` + ONNX
  diarizer (so the Windows path needs no Swift) are the follow-ups.

## Run it (macOS)

```sh
make swift-build                     # build hearsay-helper + the FluidAudio sidecars (once)
cd web && npm ci && npm run build && cd ..   # build web/dist (once)
cargo build --manifest-path rust/Cargo.toml -p hearsay-core
# from the repo root (so helper_path + web_dir resolve):
HEARSAY_SERVER_PORT=8799 ./rust/target/debug/hearsay-core            # real capture (grants mic/screen perms)
HEARSAY_SERVER_PORT=8799 ./rust/target/debug/hearsay-core --synthetic  # generated audio, no permissions
```

Open the printed `http://127.0.0.1:8799/?token=...` URL, start a meeting, and Me/Them captions stream
in live with speaker diarization; hit **Refine speakers** after stop for the offline re-diarize + higher
-accuracy re-transcription. `HEARSAY_HELPER_PATH` / `HEARSAY_REFINE_MODEL` override the helper + refine
model.

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
`### HH:MM:SS — Speaker` turns), and a `meeting.json`. The offline refine is available via
`POST /rediarize` (the "Refine speakers" button); wiring it to run automatically at stop is the one
follow-up. Finals persist to the DB — the API's source of truth — today.

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
| `hearsay-orchestrator` | Implements `LiveEngine`: spawns/supervises the capture + inference sidecars, routes 16 kHz PCM (Me/Them), owns the live transcription pipeline state machine (partials/finals), records the stereo `audio.wav` + writes `transcript.md`/`meeting.json` at stop | `src/hearsay/helper/supervisor.py`, `src/hearsay/transcript/` |
| `hearsay-capture` | Cross-platform audio capture behind one trait. `cfg(windows)`: WASAPI loopback (Them) + mic (Me) via cpal. `cfg(macos)`: Core Audio process tap (Swift helper fallback) / cpal. Resample to 16 kHz mono; monotonic `host_ts` | Swift `hearsay-helper` capture |
| `hearsay-core` | Application binary: axum HTTP + WebSocket API (loopback + session token), wires db + orchestrator + attribution, serves the React UI (or runs under Tauri) | `src/hearsay/api/`, app entrypoint |
| `hearsay-inference` | Local-only inference. **Done:** offline ASR (whisper.cpp via `whisper-rs`, GGML -> timestamped segments; CPU + `metal`/`vulkan`/`cuda`) — the accuracy harness; and the offline **refine** (`refine_them`: Swift `hearsay-diarize` + whisper re-transcribe, wired to `/rediarize`). **Next (Windows path, no Swift):** Silero VAD (ONNX) + pure-Rust Segmenter + a streaming `Transcriber` + a pure-Rust ONNX diarizer; tiered model selection by hardware | Replaces the FluidAudio Swift sidecars on the non-Mac path |

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
