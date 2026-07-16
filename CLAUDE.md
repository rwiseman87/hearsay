# Hearsay

Local-first meeting-note transcriber for macOS (Windows planned). Captures the local mic and system
audio as **separate** streams ("Me" vs "Them"), transcribes in real time, identifies the remote
speakers, and streams Markdown notes. Transcription, diarization, and the LLM run locally by default;
AWS Bedrock is configurable.

Ships as **one Rust + Tauri application** — a signed installer per OS, no interpreter bundle. The
Rust core is the single source of truth (there is no Python backend; it was removed 2026-07-14 once
the Rust port reached parity). Canonical architecture: `docs/architecture-cross-platform.md`.
Resumable task tracker: `docs/TODO.md`. IPC contract: `shared/protocol/ipc.md`.

## Architecture

Multi-process, local-only (Apple Silicon, macOS 14.4+; Windows is the remaining work):

- **Swift capture helper** (`helper/`) — the ONLY process that touches guarded native APIs (Core Audio tap,
  AVAudioEngine, ScreenCaptureKit, Vision OCR, EventKit, Accessibility). A lean PCM streamer; streams PCM +
  name hints over IPC.
- **Swift sidecars** (`helper/`, FluidAudio on the Apple Neural Engine) — the audio-AI: `hearsay-live` (live
  Them diarization + Parakeet ASR), `hearsay-me` (live Me VAD + Parakeet), `hearsay-diarize` (post-meeting
  refine), `hearsay-asr` (Parakeet, used by the refine). The core spawns + feeds each over stdio.
- **Rust core** (`rust/crates/`) — orchestration (spawns the helper + sidecars, routes PCM), speaker
  attribution (clusters + cross-meeting voiceprints + manual labels), the offline refine (whisper), LLM
  notes (later phase), Markdown, persistence, and a loopback axum HTTP + WebSocket API. The whisper
  offline ASR is the only ML it runs in-process.
- **Web UI** (`web/`) — typed React frontend served by the core, shown in a Tauri WKWebView window. The
  **Tauri shell** (`web/src-tauri/`) bundles + spawns the core and the Swift sidecars.

The core spawns and supervises the helper; they talk over two Unix sockets (binary PCM + NDJSON
control). Capture is device-local by design and cannot be centralized.

## Project Structure

```
rust/crates/
  hearsay-core/         axum HTTP+WS API: config, routes/ (thin), schema (utoipa->TS), security, state; the app binary
  hearsay-db/           SQLite via SQLx: models, queries, migrations/ (forward-only .sql)
  hearsay-orchestrator/ capture routing + Transcriber/AudioSource seams + pipeline + markdown/recorder (implements LiveEngine)
  hearsay-engine/       LiveEngine trait seam + DisabledEngine placeholder (no dependency cycle)
  hearsay-capture/      AudioSource trait + SwiftHelperSource (spawns hearsay-helper) + the TCC permissions probe
  hearsay-inference/    whisper offline ASR + the refine
  hearsay-attribution/  speaker clustering / voiceprint match / segment-speaker assignment (pure logic)
  hearsay-ipc/          binary frame codec + NDJSON control codec (source of truth for the IPC contract) + gen_fixtures bin
helper/                 SwiftPM: hearsay-{helper,live,me,diarize,asr} executables + HearsayIPC library
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
  `make fmt`, `make codegen`, `make codegen-check`, `make web-ci`, `make audit`, `make licenses`, `make ci`.
- `make ci` is the gate and must stay green: `clippy -D warnings` + `rustfmt --check` + Swift `selftest` +
  `cargo test` + codegen-drift check + `cargo audit` + `cargo deny` (license gate).
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
  (`hearsay-{live,me,diarize,asr}`) + the `HearsayIPC` library. Deployment macOS 14.4. `make swift-build`
  builds explicit products (a bare `swift build` pulls in FluidAudio's broken CLI target).
- All TCC-guarded native work lives in the capture helper (mic, audio capture, screen recording,
  accessibility, calendar); it stays lean (no FluidAudio). The heavy CoreML dep is isolated in the sidecar
  targets, so it never touches the capture binary.
- Tests run via `hearsay-helper selftest` (works with Command Line Tools); `swift test`/XCTest needs full
  Xcode. Keep the capture helper thin and stateless where possible.

## Database

- Local-first **SQLite** via **SQLx** (`sqlite://…`, WAL + `busy_timeout` + foreign keys); portable to
  PostgreSQL later.
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
- Zero-buffer watchdog: on sustained all-zero buffers, rebuild BOTH the tap and the aggregate device; emit
  `tap_health`. The mic is always "Me" and is never diarized.

## Speaker Identification (guardrails)

- Layers today: channel (Me/Them) + diarization (Them only, Swift/ANE) + cross-meeting voiceprints + manual
  labels. Calendar roster + active-speaker hints are the Phase-3 additions.
- Bind a diarization cluster -> name by **weighted majority vote** over many sparse hints (the Phase-3 design);
  a single wrong hint must never flip a stable binding. Manual labels lock a binding (votes cannot override).
- Active-speaker is **OCR-primary** (ScreenCaptureKit + Vision); Zoom Accessibility is opt-in. Degrade
  gracefully to "Speaker N" + manual labeling when hints are absent.

## Privacy & Security

- Local-first by default; raw-audio retention OFF by default; no telemetry by default.
- Minimize permissions: Microphone + Audio Capture + Screen Recording; Accessibility only for the opt-in
  Zoom path. Provide delete-meeting (DB rows + folder) + a retention setting; surface a recording-consent notice.
- Validate and sanitize all user input at API boundaries -> 422, never let the DB raise a 500.
- No secrets in code -- typed settings / macOS Keychain; redact transcripts in logs.
- NEVER add a dependency without checking its license (must be MIT, BSD, or Apache-2.0) -- `make licenses`
  (`cargo deny`, policy in `rust/deny.toml`).
- NEVER add a dependency without checking for known CVEs (`make audit` -- `cargo audit` + `npm audit`).

## Distribution

- One **signed, notarized installer per OS**, self-contained, **no CPython bundle** (Rust removes the
  hardest packaging step). NOT sandboxed / not App Store (the system-audio tap + Accessibility need it).
- Tauri handles the bundler + Apple notarization + (later) Windows signing + auto-update. `make dmg` builds
  the unsigned/ad-hoc DMG; see `docs/packaging.md`.

## Dependency Decisions

- Rust core canonical (2026-07-14): the Python FastAPI backend was removed once the Rust `hearsay-core`
  reached parity. Rust is a single self-contained artifact (no interpreter bundle), shares ~90% across
  macOS + Windows, and makes the loopback API + OpenAPI codegen the one source of truth.
- Persistence: local-first SQLite via SQLx + forward-only SQL migrations. Single-user desktop app, so no
  Postgres server; the SQLx layer keeps a future Postgres/central pivot cheap.
- macOS inference reuses the proven Swift/FluidAudio (ANE) sidecars for live ASR + diarization; the offline
  refine is whisper (`hearsay-inference`). The whisper.cpp-vs-FluidAudio unification for macOS is the one
  open, verification-gated decision (see `docs/architecture-cross-platform.md`).

## Environment Variables

- DATABASE_URL: SQLx database URL (default `sqlite://<repo>/outputs/db/hearsay.db`; portable to PostgreSQL later)
- ENVIRONMENT: development | staging | production
- HEARSAY_OUTPUT_DIR: root of the per-meeting output folders (audio, transcript, notes)
- HEARSAY_WEB_DIR: built web UI directory served when it contains `index.html`
- HEARSAY_SERVER_HOST / HEARSAY_SERVER_PORT: bind host (loopback) / port (`0` = OS-assigned)
- HEARSAY_HELPER_PATH: path to the Swift `hearsay-helper` (the `-live` / `-me` / `-diarize` sidecars resolve as siblings)
- HEARSAY_REFINE_MODEL: default GGML whisper model for the offline refine (the Settings > Models panel overrides it per install by pointing at any downloaded `ggml-*.bin`; the change applies to the next refine, no restart)
- HEARSAY_FLUID_MODELS_DIR: bundled FluidAudio live models the core seeds into FluidAudio's cache on first launch (set by the desktop shell; unset in dev, where FluidAudio downloads them)
- HEARSAY_NOTES_MODEL: default GGUF instruct model for the optional local-LLM notes step (summary + action items); empty until one is downloaded/chosen. The Settings > Models panel overrides it per install; applies to the next generate, no restart. Requires the `notes` Cargo feature (built into `rust-serve` / `dmg`)
- HEARSAY_MODELS_DIR: root the download manager writes notes models into and references them from (default `outputs/models`; the desktop shell points it at a persistent app-data dir so downloads survive reinstall)
- HEARSAY_AUTO_REFINE / HEARSAY_RECORD / HEARSAY_RECOGNITION_THRESHOLD / HEARSAY_NOTES: defaults for the editable settings sections (`HEARSAY_NOTES` toggles the notes step, default off)
