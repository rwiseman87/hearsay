# Test suite

Hearsay's tests run **on demand from the Makefile** — no hosted CI, no timers, no git hooks. Rust is
the source of truth for the API + IPC contract, and the tests consume that contract rather than
re-implementing it. There are two tiers:

- a **deterministic gate** (`make ci`) — fast, hardware-independent, model-free; the pre-flight before
  a commit or PR.
- **model/hardware probes** (`make probes`) — the `#[ignore]`d tests that need a downloaded model and
  the ANE/GPU; run on a box that has them.

This document maps the targets to what they test and the suite to where it lives. For the quickstart
(how to run each) see [development.md](development.md#testing).

## Targets — what each runs and tests

`make ci` is an aggregate of the leaf targets below; each leaf is also runnable on its own.

| Target | Runs | Tests | Models/HW? |
|---|---|---|---|
| **`make ci`** | `lint test tauri-test codegen-check version-check audit licenses web-ci` | the whole deterministic gate (everything below except e2e/probes/coverage) | no |
| `lint` | `rust-lint` (clippy `--all-targets -D warnings` + `rustfmt --check` on `rust/`) + `tauri-lint` (same on `web/src-tauri`) | no warnings, formatted | no |
| `test` | `swift-build` + `swift-test` + `rust-test` | Swift codec parity + the full Rust workspace | no |
| `swift-test` | `hearsay-helper selftest` against `shared/fixtures/` | the Swift `FrameCodec` matches the golden IPC frames byte-for-byte | no |
| `rust-test` | `cargo test` over the `rust/` workspace | every non-`#[ignore]` Rust test (units, DB, HTTP API, full-stack, orchestrator, IPC, regression locks) | no |
| `tauri-test` | `cargo test` on `web/src-tauri` | the shell's pure logic (e.g. `html_escape` on the untrusted error `detail`) | no |
| `codegen-check` | regenerate IPC fixtures + OpenAPI + TS, then `git diff --exit-code` | the committed `shared/fixtures/*`, `web/openapi.json`, and `web/src/api/schema.ts` have not drifted from the Rust source | no |
| `version-check` | compare version strings | `rust/Cargo.toml` (canonical), `tauri.conf.json`, and `web/package.json` agree | no |
| `audit` | `cargo audit` (both Rust trees) + `npm audit` | no known CVEs in dependencies | no |
| `licenses` | `cargo deny check licenses` (policy `rust/deny.toml`) | every dependency is MIT/BSD/Apache-2.0 | no |
| `web-ci` | `web-install` + `web-typecheck` (tsc) + `web-lint` (eslint) + `web-test` + `web-build` | the web UI type-checks, lints, unit/component-tests, and builds | no |
| `web-test` | `vitest` (jsdom) | the client layer + hooks + MSW-mocked components (also folded into `web-ci`) | no |
| **`make e2e`** | build the core, then Playwright/Chromium vs the scripted core + `vite dev` | the real React app → real core → real pipeline over a live WebSocket | no (Chromium once) |
| **`make probes`** | `cargo test -- --ignored` on `hearsay-inference` (metal), `hearsay-notes` (metal), `hearsay-backends`, `hearsay-capture` | the ML paths: whisper refine + diarize, the notes LLM, the live pipeline, capture | **yes** |
| `make coverage` | `cargo-llvm-cov` + vitest v8 → `outputs/coverage/` | report-only; the "what's untested" view | no |
| **`make test-all`** | `ci` + `probes` + `e2e` | everything, on a fully-equipped box | yes |
| `make clean-test` | `rm -rf outputs/coverage outputs/e2e` | (removes report dirs; test *data* auto-cleans via tempdirs) | no |

`make ci` + `make e2e` is the complete deterministic suite on a machine without the models. Windows
has no `make`, so the same set is mirrored in `scripts\test-windows.ps1`
(`-Target ci|web|tauri|probes|coverage|e2e|all`) with the Windows feature set.

## The suite by layer

| Layer | What's tested | Where | Run by |
|---|---|---|---|
| **Pure-logic units** | speaker ordering / segment-speaker assignment / voiceprint matching (`hearsay-attribution`); notes prompt build + reply parse (`hearsay-notes-prompt`); IPC frame + control codec (`hearsay-ipc`); inline core/orchestrator/inference units (AEC, recorder gap-fill, markdown render) | `#[cfg(test)]` mods in each crate's `src/` | `rust-test` |
| **Database** | schema round-trips over real migrations, `queries.rs` branches (FTS search, cross-meeting edit scope, the `reassign_segment_speaker` guards, cycle guard), the migration-upgrade fixture (populated old DB → head, data intact) | `hearsay-db/tests/schema.rs` (in-memory SQLite) | `rust-test` |
| **Core HTTP API** | the assembled axum router via `tower::oneshot` against in-memory SQLite with `DisabledEngine` — every REST route: meetings, segments (edit + per-line speaker reassign), speakers, folders, notes, search, settings, auth/Origin gating | `hearsay-core/tests/api.rs` | `rust-test` |
| **Full-stack HTTP/WS** | the core booted on a real TCP port with a scripted orchestrator: `start` → live `TranscriptEvent` frames over a real WS client → `stop` → REST read-back; a companion refine read-back replaces the Them track | `hearsay-core/tests/api.rs` | `rust-test` |
| **Orchestrator** | meeting lifecycle, the capture→transcribe→persist pipeline, recorder/markdown, refine — all with scripted fakes (fake audio source + stubbed transcribers/diarizer, no ML) | `hearsay-orchestrator/tests/`, `src/testing.rs` | `rust-test` |
| **Web client + hooks** | `api/client.ts` (request shaping, token, timeout, error envelope), `api/ws.ts` (reconnect/backoff, parse), the query hooks + `queryKeys`, `hooks/useTranscript.ts` (live assembly) against a stubbed fetch/WebSocket | `web/src/**/*.test.ts(x)` | `web-test` |
| **Web components** | `TranscriptView` (line edit + speaker reassign), `Library` (list/select/delete), `SettingsPage` (models validation) against the real fetch wrapper + MSW | `web/src/components/*.test.tsx` | `web-test` |
| **Browser E2E** | real UI → real core (scripted engine) → real pipeline: start → live transcript → stop → Library → rename speaker → reassign a line → generate notes, asserting exact text | `web/e2e/meeting.spec.ts` | `e2e` |
| **IPC parity** | the codec against golden fixtures, in **both** languages | `hearsay-ipc/tests/golden_fixtures.rs` + Swift `selftest` | `rust-test`, `swift-test` |
| **Regression locks** | `insta` snapshot of the markdown export; `proptest` for the IPC codec round-trip + voiceprint `cosine`; codegen-drift diff | across the crates above + `codegen-check` | `rust-test`, `codegen-check` |
| **Model/hardware probes** | whisper refine + `hearsay-diarize` (`refine_mac_probe`, `transcribe`, embed-cap), the notes LLM (`summarize`), the live pipeline (`streaming_pipeline`), synthetic capture | `*/tests/*probe*.rs`, `hearsay-{notes,backends,capture}/tests/` (all `#[ignore]`d) | `probes` |

## Fakes and seams

The gate stays model-free because the ML is replaced at well-defined seams:

- **`DisabledEngine`** — answers every capture route with 503 / a clean close; the `oneshot` API tests
  run against it, so the router is exercised with no engine at all.
- **Scripted orchestrator fakes** (`hearsay-orchestrator/src/testing.rs`) — a fake `AudioSource` plus
  stubbed transcribers/diarizer that replay a canned meeting. The full-stack Rust test wires these into
  a real `Orchestrator` + `AppState` in-process.
- **`build_scripted_engine` + the dev-only `HEARSAY_SCRIPTED` flag** — the same scripted engine selected
  inside the core *binary* (honored only when `ENVIRONMENT=development`, so a shipping build never fakes
  a meeting). This is what the browser E2E drives, streaming the transcript progressively over the live
  WebSocket, so both the Rust full-stack test and Playwright assert identical output every run.
- **`memory_pool()`** — a single-connection `sqlite::memory:` pool running the real migrations, shared
  by the DB and API tests.
- **MSW** — mocks the HTTP API for the React component tests while the real fetch wrapper runs.

## Isolation and artifacts

Every test that touches disk points `HEARSAY_OUTPUT_DIR` + `DATABASE_URL` at a `tempfile::tempdir()`
(or in-memory SQLite), so a run never writes the real `outputs/db/` or `outputs/recordings/` and leaves
the working tree untouched; `TempDir` cleans itself on drop (including on panic). The full-stack Rust
test serves the core in-process on an ephemeral port (no external process to reap); the browser E2E is
the one case that spawns a real process, managed by Playwright's `webServer` config. Triage artifacts —
coverage (`outputs/coverage/`) and Playwright reports/traces (`outputs/e2e/`) — land only under the
gitignored `outputs/`; `make clean-test` removes them.

## Platform parity (macOS / Windows)

Hearsay is one codebase with per-OS edges: ~90% is shared and must behave identically; capture, live
ASR/diarization, the refine GPU, and packaging differ by design (see
[windows-port.md](windows-port.md)). The shared surface — core API, DB, orchestrator, attribution,
notes-prompt, markdown, the full-stack HTTP/WS test, and the browser E2E — runs the **same tests on both
OSes** and is the parity backbone. The real differences are asserted where they are real: capture
(`SwiftHelperSource` vs `WasapiSource`), live ASR/diarization (FluidAudio/ANE vs `sherpa`), and the
refine GPU (`metal` vs `vulkan`/CPU) are covered per-OS by unit tests for the pure logic and by
`make probes` for the model paths. IPC codec parity and the Swift `selftest` are macOS-only by design
(no Swift/helper on Windows). Windows runs the whole set through `scripts\test-windows.ps1`.

## Running the probes

`make probes` is not model-free: each probe hard-requires a downloaded model, and some need an input
WAV supplied via environment variable (a bare `make probes` fails on the first such probe by design).
Point them at real inputs — for example the refine decomposition probe:

```sh
HEARSAY_BENCH_WAV=outputs/recordings/<meeting>/audio.wav \
HEARSAY_REFINE_MODEL=outputs/models/ggml-large-v3-turbo.bin \
HEARSAY_DIARIZE_BIN=helper/.build/arm64-apple-macosx/release/hearsay-diarize \
HEARSAY_NOTES_MODEL=outputs/models/<instruct>.gguf \
make probes
```

The GPU-backed probes that ship their own fixtures (e.g. `refines_real_meeting_them_track`,
`summarize`) need only the models; the WAV/diarizer vars are for the decomposition/benchmark probes.
The refine anti-loop entropy thresholds are pinned in `hearsay-inference/tests/refine_mac_probe.rs` so a
whisper repetition-attractor regression surfaces here.

## Not covered (by design)

- **The packaged macOS `.app`** cannot be driven end-to-end: Apple ships no WebDriver for `WKWebView`.
  The Playwright path drives the identical web bundle against an identical core, so only the real
  WKWebView runtime + the shell↔core boot handshake are left to manual smoke (the Rust side of that
  handshake is unit-tested). Windows *can* drive its packaged app via `tauri-driver` (WebView2) — a
  Windows-only parity gain, not yet wired up.
- **Real capture devices** (mic, system-audio tap, screen recording) need TCC permissions and hardware;
  the pure device-selection/format/gap-fill logic is unit-tested, the live devices are manual smoke.
