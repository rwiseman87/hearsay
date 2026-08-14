# Hearsay

Local-first meeting-note transcriber for macOS and Windows. Captures the local mic and system
audio as **separate** streams ("Me" vs "Them"), transcribes in real time, identifies the remote
speakers, and streams Markdown notes. Transcription, diarization, and the notes LLM all run on-device;
audio never leaves the machine.

Ships as **one Rust + Tauri application** — one installer per OS, no interpreter bundle. The
Rust core is the single backend and the single source of truth.
Canonical architecture: `docs/architecture.md`. Design rationale: `docs/design-decisions.md`.
IPC contract: `shared/protocol/ipc.md`. `README.md` is the single documentation index; there is no
`docs/README.md`.

## Architecture

Multi-process, local-only (macOS on Apple Silicon 14.4+; Windows on x86_64, Win10 2004+):

- **Swift capture helper** (`helper/`) — the ONLY process that touches guarded native APIs (the Core
  Audio process tap and the microphone via AVAudioEngine). A lean PCM streamer; streams PCM and device
  hints over IPC.
- **Swift sidecars** (`helper/`, FluidAudio on the Apple Neural Engine) — the audio-AI: `hearsay-live` (live
  Them diarization + Parakeet ASR), `hearsay-me` (live Me VAD + Parakeet), `hearsay-diarize` (post-meeting
  refine). The core spawns + feeds each over stdio.
- **Rust core** (`rust/crates/`) — orchestration (spawns the helper + sidecars, routes PCM), speaker
  attribution (clusters + cross-meeting voiceprints + manual labels), the offline refine (whisper),
  optional local-LLM notes (spawned as the `hearsay-notes` sidecar, off by default), Markdown,
  persistence, and a loopback axum HTTP + WebSocket API. The whisper refine is the only ML it runs
  in-process; the notes LLM (llama.cpp) runs out-of-process because llama's and whisper's vendored
  `ggml` collide when co-linked (a ~5x refine slowdown).
- **Rust notes sidecar** (`hearsay-notes`) — the local-LLM notes step (llama.cpp), a standalone binary
  the core spawns over stdio. Separate process for the `ggml` reason above; the pure
  prompt/parse logic is shared via the dependency-free `hearsay-notes-prompt` crate.
- **Web UI** (`web/`) — typed React frontend served by the core, shown in a Tauri WKWebView window. The
  **Tauri shell** (`web/src-tauri/`) bundles + spawns the core, and bundles the Swift sidecars + the
  `hearsay-notes` sidecar.

The core spawns and supervises the helper; they talk over two Unix sockets (binary PCM + NDJSON
control). Capture is device-local by design and cannot be centralized.

## Project Structure

```
rust/crates/
  hearsay-core/         axum HTTP+WS API: config, routes/ (thin), schema (utoipa->TS), security, state; the app binary
  hearsay-db/           SQLite via SQLx: models, queries, migrations/ (forward-only .sql)
  hearsay-orchestrator/ capture routing + Transcriber/AudioSource seams + pipeline + markdown/recorder + notes seam (implements LiveEngine)
  hearsay-engine/       LiveEngine trait seam + DisabledEngine placeholder (no dependency cycle)
  hearsay-backends/     per-OS backend wiring: MacBackend/MacRefiner + SubprocessSummarizer + build_engine (rediarize + notes)
  hearsay-capture/      AudioSource trait + SwiftHelperSource (spawns hearsay-helper) + the TCC permissions probe
  hearsay-inference/    whisper offline ASR + the refine (whisper-rs; no llama — see hearsay-notes)
  hearsay-notes/        the local-LLM notes sidecar (llama-cpp-2); spawned by the core, kept out of its binary
  hearsay-notes-prompt/ dependency-free prompt build + reply parse, shared by the core default + the notes sidecar
  hearsay-attribution/  speaker clustering / voiceprint match / segment-speaker assignment (pure logic)
  hearsay-audio/        lossless FLAC archival of the recorded meeting wav (encode + decode + byte-exact verify)
  hearsay-ipc/          binary frame codec + NDJSON control codec (source of truth for the IPC contract) + gen_fixtures bin
helper/                 SwiftPM: hearsay-{helper,live,me,diarize} executables + HearsayIPC + SidecarIO libraries
web/                    React + TS frontend; web/src-tauri/ is the Tauri desktop shell
shared/protocol/ipc.md  IPC contract (source of truth)   ·   shared/fixtures/   golden frames (Rust-generated)
```

## Code Conventions

- Rust: `cargo fmt` clean, `cargo clippy --all-targets -- -D warnings` clean. Errors surface through
  `ApiError` (never leak SQL to a client). Prefer the standard library; add a dependency only for clear
  value, at a pinned exact version.
- Do not add docstrings, comments, or type hints to code you did not change. Comments only where logic is
  non-obvious.
- Frontend: TypeScript strict mode, functional components. Native Fetch API -- no Axios; one canonical
  fetch wrapper carrying the session token.
- All list endpoints return paginated responses: `{ total, page, page_size, items }`.
- Swift: 4-space indent; `swift build` clean. Native frameworks only in the helper (see Swift Helper).

## Build, Test & Tooling

- **cargo** for Rust, **npm** for the web UI. The **Makefile** is the task runner.
- Targets: `make rust-build`, `make test` (Swift selftest + cargo test), `make lint` (clippy + rustfmt),
  `make fmt`, `make codegen`, `make codegen-check`, `make web-ci`, `make audit`, `make licenses`, `make ci`,
  `make diarize-eval` (offline diarization accuracy gate: speaker-count + DER vs a committed baseline over a
  local labeled corpus; self-skips inside `make ci` when the audio/sidecar are absent).
- `make ci` is the gate and must stay green (`ci: lint test tauri-test codegen-check version-check audit
  licenses web-ci`): `clippy -D warnings` + `rustfmt --check` + Swift `selftest` + `cargo test` + the Tauri
  shell's clippy/tests + codegen-drift check + app-version drift check + `cargo audit` + `cargo deny`
  (license gate) + the web gate (`tsc` + ESLint + vitest + `vite build`).
- Run the core locally: `make rust-serve` (`SYNTHETIC=1` for no-permission plumbing). Build the app:
  `make dmg` (see `docs/packaging.md`).
- Pin exact versions in lockfiles (`rust/Cargo.lock`, `web/package-lock.json`). npm: `ignore-scripts=true`.

## IPC Contract

- `shared/protocol/ipc.md` is the single source of truth: a fixed 28-byte little-endian media-frame
  header + payload on `media.sock`, and NDJSON commands/events on `control.sock`.
- The Rust `hearsay-ipc` codec and Swift `HearsayIPC.FrameCodec` MUST match byte-for-byte.
- `shared/fixtures/frames.jsonl` pins the contract; it is generated from `hearsay-ipc`
  (`cargo run -p hearsay-ipc --bin gen_fixtures`, wired into `make codegen`) and validated in CI by both
  languages (`cargo test` golden fixtures + `hearsay-helper selftest`). Never hand-edit fixtures.

## Swift Helper

- SwiftPM package in `helper/`: the `hearsay-helper` capture executable + the FluidAudio/ANE sidecars
  (`hearsay-{live,me,diarize}`) + the `HearsayIPC` + `SidecarIO` libraries. Deployment macOS 14.4. `make swift-build`
  builds one product per invocation (`swift build` takes a single `--product`, and a bare
  `swift build` pulls in FluidAudio's CLI target).
- All TCC-guarded native work lives in the capture helper (mic + audio capture); it stays lean (no
  FluidAudio). The heavy CoreML dep is isolated in the sidecar targets, so it never touches the
  capture binary.
- Tests run via `hearsay-helper selftest` (works with Command Line Tools); `swift test`/XCTest needs full
  Xcode. Keep the capture helper thin and stateless where possible.

## Database

- Local-first **SQLite** via **SQLx** (`sqlite://…`, WAL + `busy_timeout` + foreign keys).
- Models + queries: `rust/crates/hearsay-db/` -- rows use UUID primary keys and `created_at`/`updated_at`.
- Migrations: `rust/crates/hearsay-db/migrations/` -- forward-only numbered `.sql`, applied by the embedded
  SQLx `Migrator` on startup. Add one as `000N_description.sql`.
- Never build SQL from string interpolation (SQLx binds parameters). Wrap multi-step writes in an explicit
  transaction (`pool.begin()`).

## API & Web

- Bind the core to **127.0.0.1 only** and require a **per-session bearer token** on REST + WebSocket
  (loopback is not a security boundary); enforce an Origin/Host allowlist. Minimal CSP in the webview.
- API routers are thin -- validate input, call a query / the engine, return a response. Business logic
  lives in `hearsay-orchestrator` / the query layer.
- Backend types are codegen'd from the Rust OpenAPI (utoipa, `hearsay-core --dump-openapi`) into
  `web/openapi.json` -> `web/src/api/schema.ts`. `make codegen-check` fails CI on drift, so a feature that
  reaches the frontend but not the shipping core is caught at CI, not at runtime.
- **The OpenAPI document is the endpoint reference.** Every route carries a doc comment on its
  handler (first line -> `summary`, the rest -> `description`) and a `description` on each response
  status. That prose is the documentation -- it reaches `schema.ts` as JSDoc and renders in the
  console. A new route without it ships an undocumented endpoint, so write it with the handler.
  `docs/api.md` covers only what a schema cannot express: auth, conventions, the audio byte stream,
  and the WebSocket protocol.
- Browsable console at `/docs` under the `api-console` feature + `ENVIRONMENT=development`
  (`make rust-serve` enables it); off elsewhere so the shipping binary carries no Swagger assets.

## Testing

- `cargo test`: unit tests for pure logic + integration tests over the assembled axum router
  (`tower::ServiceExt::oneshot`) against an in-memory SQLite DB, with the capture routes on `DisabledEngine`.
- The diarization mapping (`order_speakers`, `assign_segment_speaker`) + voiceprint matching are pure logic
  in `hearsay-attribution` -- unit-test them directly; the pipeline / sidecar processors / refine are tested
  with scripted fakes (no ML deps).
- Swift codec parity: `hearsay-helper selftest` against the golden fixtures. Run all: `make test`.

## Audio Capture (guardrails)

- System audio: Core Audio process tap configured **global-except-self** (dodges the Teams
  per-process-silent bug; also covers browser meeting apps).
- Resample both sources to **16 kHz mono**; stamp both with ONE monotonic clock (`host_ts`). Cross-stream
  alignment is by timestamp, never by sample index.
- Both capture streams die two ways, and a watchdog must cover BOTH axes: *cadence* (buffers stop
  arriving) and *amplitude* (buffers keep arriving at full rate carrying only exact zeros). Amplitude
  alone cannot tell a dead tap from quiet system audio — gate it on
  `outputDeviceIsRunningSomewhere()`, and prefer a needless rebuild over a missed one. On a sustained
  zero run, rebuild BOTH the tap and the aggregate device and emit `tap_health`; the mic restarts its
  `AVAudioEngine` and emits `mic_health`. Detection without a recovery action is not a watchdog.
- The mic is always "Me" and is never diarized.
- Echo cancellation (`aec` feature, built into `rust-serve` + the bundle): SpeexDSP (`aec-rs`) cancels the
  Them playback out of the live Me stream. The recorder and offline refine read the RAW pre-AEC audio; only
  live captions see the cancelled Me stream. See `docs/echo-cancellation.md`.

## Speaker Identification (guardrails)

- Layers: channel (Me/Them) + diarization (Them only, Swift/ANE) + cross-meeting voiceprints + manual
  labels. Cross-meeting recognition matches a cluster's voiceprint centroid to prior locked speakers by
  cosine similarity above a threshold.
- Manual labels lock a binding (recognition cannot override a locked name). Manual correction works at
  three granularities: renaming a whole cluster (locks the binding), reassigning a single line to
  another/new speaker (segment-level `cluster_id` override, flagged `edited`), and merging one cluster
  into another (bulk reassign + drop the source, flagged `edited`, meeting-scoped, freed ordinal never
  reused).
- Stored voiceprints are user-manageable (Settings > Voices): list them per person with their
  per-meeting samples, rename a person everywhere (`409` on a name collision — never a silent identity
  merge), and forget one sample or all of them. Forgetting sets `centroid = NULL` and nothing else, so
  names, `locked`, and past transcripts survive and only future recognition stops. The roster lists
  people who HAVE an embedding, not every known name — a name with no centroid never appears, and
  clearing someone's last sample drops them from it.
- Degrade gracefully to "Speaker N" + manual labeling when no name is known.

## Privacy & Security

- Local-first by default; raw-audio retention OFF by default; no telemetry by default.
- Minimize permissions: Microphone + Audio Capture only. Provide delete-meeting (DB rows + folder) +
  a retention setting; surface a recording-consent notice.
- Validate and sanitize all user input at API boundaries -> 422, never let the DB raise a 500.
- No secrets in code -- typed settings / macOS Keychain; redact transcripts in logs.
- NEVER add a dependency without checking its license (must be MIT, BSD, or Apache-2.0) -- `make licenses`
  (`cargo deny`, policy in `rust/deny.toml`).
- NEVER add a dependency without checking for known CVEs (`make audit` -- `cargo audit` + `npm audit`).

## Distribution

- One **self-contained installer per OS**, no interpreter bundle (Rust removes the hardest packaging
  step). NOT sandboxed / not App Store (the system-audio tap needs it). `make dmg` builds the
  ad-hoc-signed, un-notarized DMG; `scripts/build-windows.ps1` builds the NSIS installer.

## Dependency Decisions

- Rust core canonical: a single self-contained artifact with no interpreter bundle,
  ~90% shared across macOS + Windows, with the loopback API + OpenAPI codegen as the one source of
  truth.
- Persistence: local-first SQLite via SQLx + forward-only SQL migrations. Single-user desktop app, so
  there is no database server to run.
- macOS inference uses the Swift/FluidAudio (ANE) sidecars for live ASR + diarization; the offline
  refine is whisper (`hearsay-inference`). Windows uses the sherpa-onnx live path behind the same seams.

## Environment Variables

Every variable, with its default, is tabulated in `docs/configuration.md`, alongside the writable
settings overlay and its validation rules. The compiler-checked source is `Settings` in
`rust/crates/hearsay-core/src/config.rs`; the table mirrors it. Rationale for a given default lives
with the behavior it governs, not in the table.

All configuration is resolved from the environment at startup into that one typed struct. A
malformed override is a warning in development and a hard startup error otherwise.
