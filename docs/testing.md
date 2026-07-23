# Test suite buildout

The plan and tracking state for growing Hearsay's automated tests into a comprehensive
suite — unit, integration, and regression — run on demand from the Makefile on the maintainer's
own machines, not a hosted CI service and not on a timer.

This document is the working tracking state: the phase checkboxes below are updated as work
lands. The durable "how to test" reference stays in [development.md](development.md#testing);
once a phase ships, its lasting parts fold into that section. Design rationale is recorded here
so it is not re-litigated.

## Why this doc

The building blocks are already strong (see [Baseline](#baseline)), but two things are missing:

1. **Whole surfaces have no tests at all** — the entire web/Tauri frontend, the live Swift
   pipelines, the WebSocket push path, crash-recovery, and the model paths (only exercised by
   `#[ignore]`d probes that never run).
2. **There is no single on-demand entry point for the full suite.** `make ci` is the fast
   deterministic gate, but it has no web tests, skips the model probes, and produces no coverage.
   The goal is a small set of Makefile targets — deterministic gate, model/hardware probes,
   coverage, and one "run everything" target — that the maintainer invokes when they want, on
   their own macOS and Windows machines. No hosted CI, no timers, no git hooks.

## Baseline

What exists today, so the plan builds on it rather than duplicating it.

| Area | Coverage | Where |
|---|---|---|
| Core HTTP API | 42 integration tests over the assembled axum router (`tower::oneshot`, in-memory SQLite, `DisabledEngine`) | `hearsay-core/tests/api.rs` |
| Database | 26 tests over in-memory SQLite running real migrations | `hearsay-db/tests/schema.rs` |
| Orchestrator | 17 lifecycle tests + a scripted-fakes library; inline AEC/pipeline/recorder units | `hearsay-orchestrator/tests/`, `src/testing.rs` |
| Pure logic | attribution (26), notes-prompt (9) | `hearsay-attribution`, `hearsay-notes-prompt` |
| IPC contract | codec units + cross-language golden parity (Rust test **and** Swift `selftest`) | `hearsay-ipc`, `shared/fixtures/` |

Strong. The gaps below are everything *outside* this table.

## Platform coverage & parity

Hearsay is one codebase with per-OS edges: ~90% is shared and must behave identically; capture, live
ASR/diarization, the refine GPU, and packaging differ by design (see
[windows-port.md](windows-port.md)). The suite runs on both the macOS (ANE/Metal) and Windows
(sherpa/Vulkan) machines. The parity goal: the shared surface passes identically on both, and the
real differences are asserted where they are real, not papered over.

| Surface | macOS | Windows | Test strategy |
|---|---|---|---|
| Core API, DB, orchestrator, attribution, notes-prompt, markdown | shared | shared | **same tests, both OSes** — the parity backbone |
| Full-stack HTTP/WS integration + browser E2E (scripted engine, P4) | yes | yes | model-free + platform-neutral → literally the same tests |
| Capture (`AudioSource`) | Swift helper (sockets) | in-process `WasapiSource` | per-OS: loopback-mode/format/gap-fill logic unit-tested; real devices = manual smoke |
| Live ASR / diarization | FluidAudio/ANE sidecars | `SherpaTranscriber` (`sherpa`) | per-OS model probes (`make probes`) |
| Live speakers | diarized live | "Speaker N" live, real at refine | **intentional difference** — parity assertions must not require live Windows diarization |
| Refine | whisper + `hearsay-diarize` sidecar | whisper + in-process `SherpaDiarizer` | shared `assemble`/`parse` logic unit-tested; GPU path = probe |
| Refine GPU | `metal` | `vulkan` / CPU | probe per platform; the empty-Vulkan-device CPU fallback is currently unexercised (noted in windows-refine-crash.md §4) |
| IPC codec / Swift `selftest` | yes (cross-language parity) | n/a (no Swift, no helper) | macOS-only by design |
| Packaged-app E2E | **not possible** (no `WKWebView` WebDriver) | **possible** via `tauri-driver` (WebView2/Edge) | a Windows-only parity gain |
| Notes sidecar | shared | shared | same tests |

Two consequences the parity goal forces up front:

- **The shared suite must actually pass on Windows.** Two tests fail there today
  (windows-refine-crash.md §4): `permissions_probe_degrades_when_helper_missing` asserts macOS-only
  helper behavior (Windows has no helper, so `win_permissions` returns `available: true`) and must be
  `cfg`-gated; and `storage_override_pins_meeting_dir_off_the_default_root` fails undiagnosed — a
  possible real Windows path bug. Green-on-both is Phase 2 work, not an afterthought.
- **The entry point must exist on Windows.** Windows has no `make` (it builds via
  `scripts\build-windows.ps1`), so the targets are mirrored in `scripts\test-windows.ps1` running the
  same `cargo`/`npm`/Playwright commands with the Windows feature set (`--features sherpa,notes`, and
  `sherpa,vulkan` for the GPU probe). `hearsay-inference/build.rs` already copies the sherpa/onnx
  DLLs next to the test binaries, so `cargo test` there needs only the features.

An in-process fragility unique to Windows shapes the isolation strategy: unlike macOS, which runs
diarization in the crash-isolated `hearsay-diarize` sidecar, the Windows backend runs the diarizer
**in-process**, so an uncaught onnx exception aborts core + refine + API together (windows-refine-crash.md
§1). That is why the refine-crash regression can only assert "absence of death," not `should_panic`.

## Principles

- **On-demand via `make`, nothing automatic.** The suite is a handful of Makefile targets the
  maintainer runs when they want. No hosted CI, no timers/`launchd`/Task Scheduler, no git hooks.
  Windows has no `make`, so the same targets are mirrored in `scripts\test-windows.ps1`.
- **Deterministic gate vs. model-gated probes.** Fast, hardware-independent tests live in `make ci`
  (which now includes the web and Tauri unit tests). Anything needing a downloaded model or
  specific hardware stays `#[ignore]`d and runs only via `make probes`, where the models and the
  ANE/GPU exist.
- **New test deps clear the existing gates.** Every added crate/package must pass the license gate
  (MIT/BSD/Apache-2.0 via `rust/deny.toml`) and `make audit`, and be pinned to an exact version.
- **Rust stays the source of truth.** Codegen drift (`make codegen-check`) already guards the
  OpenAPI → TS and IPC fixtures; new tests do not re-implement that contract, they consume it.

## Make targets

The suite is composed from these on-demand targets (new ones added by the phases below).

| Target | Runs | Needs models/HW? |
|---|---|---|
| `make ci` | the deterministic gate — lint, Rust + Swift + **web + Tauri** unit/integration tests, codegen drift, version, audit, licenses | no |
| `make web-test` | web unit/component tests (vitest); also folded into `web-ci` | no |
| `make tauri-test` | `cargo test` on `web/src-tauri` | no |
| `make e2e` | browser end-to-end (Playwright) against a running core + vite | no (needs browser binaries) |
| `make probes` | the `#[ignore]`d model/hardware tests (`cargo test -- --ignored`) | yes |
| `make coverage` | `cargo-llvm-cov` + vitest coverage → `outputs/coverage/` | no |
| `make test-all` | `make ci` + `make e2e` + `make probes` — the one "run everything" target | yes |
| `make clean-test` | remove the report dirs (`outputs/coverage`, `outputs/e2e`); test *data* auto-cleans | no |

`make ci` stays the fast pre-flight (and includes the full-stack HTTP/WS integration test, which
needs no models); `make test-all` is the comprehensive on-demand run. On a box without the models,
`make ci` + `make e2e` is the complete deterministic suite on its own. Windows runs the same set
through `scripts\test-windows.ps1` (see [Platform coverage & parity](#platform-coverage--parity)).

## Results & cleanup

The suite is designed to leave the working tree exactly as it found it.

**Isolation.** Every test that touches disk points `HEARSAY_OUTPUT_DIR` and `DATABASE_URL` at a
`tempfile::tempdir()` (or in-memory SQLite) — the convention the current `lifecycle.rs` /
`end_to_end.rs` tests already follow. Nothing writes the maintainer's real `outputs/db/hearsay.db`
or `outputs/recordings/`. `TempDir` removes itself on drop, including on panic, so test *data* needs
no explicit cleanup. The browser E2E core + vite run against a temp output dir and OS-assigned
ephemeral ports for the same reason.

**Process teardown (the one new risk).** Unlike a `TempDir`, a spawned OS process is not reaped by a
panicking test. The full-stack integration and E2E harnesses each own an RAII guard whose `Drop`
kills the spawned process group (core → sidecars/notes) and waits, so an aborted or failed run leaves
no zombie process and frees the port. Playwright uses its `webServer` config to start and stop the
core + vite around the run.

**Results.** Default reporters go to the terminal (cargo, vitest, Playwright) — pass/fail and
timings, which is all the gate needs. Artifacts for *triage* are written only under the
already-gitignored `outputs/` (matching the repo's scratch convention):

| Artifact | Path | Retention |
|---|---|---|
| Coverage (lcov + HTML) | `outputs/coverage/` | overwritten each `make coverage`; `cargo llvm-cov clean` first |
| Playwright HTML report | `outputs/e2e/playwright-report/` | last run |
| Playwright trace / screenshot / video | `outputs/e2e/test-results/` | **captured on failure only**, for the trace viewer |

`make coverage` is the "what's untested" view; `make clean-test` purges both dirs and is folded into
`make clean`.

---

## Phase 0 — On-demand entry points (Makefile)

Add the targets that compose the suite. No new test logic yet — this is the scaffolding the later
phases hang their tests on, and it makes the model probes runnable at all.

- [x] `make probes` — `cargo test -- --ignored` across `hearsay-inference`, `hearsay-notes`,
      `hearsay-backends`, `hearsay-capture` (mac uses `--features metal`; the crate-clean-skip on
      absent models is Phase 3's baselining work).
- [x] `make test-all` = `make ci` + `make probes`.
- [x] `make coverage` — `cargo-llvm-cov` (workspace) + vitest v8, report-only into `outputs/coverage/`
      (`cargo-llvm-cov` is a prerequisite; the target preflights it with an install hint).
- [x] `make web-test` (wired into `web-ci` now) and `make tauri-test` (defined; wired into `ci` with
      the Tauri shell test in Phase 2).
- [x] `scripts\test-windows.ps1` — the PowerShell mirror (`-Target ci|web|tauri|probes|coverage|all`),
      running the same commands with the Windows feature set (`sherpa`, plus `vulkan` for the GPU
      probes; `notes` is not a workspace feature — the notes sidecar is a separate crate).
- [x] Document the target set (and the Windows mirror) in [development.md](development.md#testing).

## Phase 1 — Web test harness

There is no web test runner today. Stand one up and cover the highest-risk client code.

- [x] Add dev deps (exact-pinned, license-checked): `vitest`, `@testing-library/react`,
      `@testing-library/dom` (peer), `@testing-library/user-event`, `jsdom`, `msw`,
      `@vitest/coverage-v8`. (`vitest-axe` deferred with the a11y rule.)
- [x] Add a `test` block to `web/vite.config.ts` (jsdom env, v8 coverage) and `test` / `test:watch`
      / `coverage` scripts to `web/package.json`.
- [x] Wire `web-ci` to run `npm run test` after typecheck/lint.
- [x] **Unit — the client layer (highest risk):**
  - [x] `api/client.ts` — request shaping, bearer-token header, timeout/abort composition,
        error-envelope normalization. (No FormData path exists — the wrapper is JSON-only, so that
        case is not tested.)
  - [x] `api/ws.ts` — connect/reconnect (exponential backoff + reset-on-open), token url-encoding,
        message parse + malformed-drop, resync routing.
  - [x] `api/hooks.ts` + `api/queryKeys.ts` — `useSegments` pagination, `setQueryData` cache patch vs
        broad invalidation, query-key factory. Plus `api/token.ts` (global vs `?token=` fallback).
  - [x] `hooks/useTranscript.ts` — live transcript assembly (seed merge/replace, final-supersedes-
        partial, status/prompt/health/pause flags, level routing, ordering, reset on meeting change).
- [ ] **Component (MSW-mocked API):** `SettingsPage` validation, `TranscriptView` editing, `Library`
      list/actions. Component-level (mocked backend) — the real-backend flows live in Phase 4.

## Phase 2 — Rust unit + integration fill

Close the untested backend surfaces, reusing the existing fakes (`DisabledEngine`, scripted
sidecars, `memory_pool()`).

- [x] **WebSocket path (`hearsay-core/src/routes/ws.rs`)** — handshake gating done with a real WS
      client (`tokio-tungstenite`) against a server on an ephemeral port: non-loopback Origin -> 403,
      missing/wrong token -> 401, valid loopback token -> 101 (in `api.rs`). `oneshot` cannot drive it
      (`WebSocketUpgrade` runs before the handler body -> 426). **Deferred:** broadcast of the three
      event types via a scripted engine — folds into the Phase 4 full-stack harness (same real server).
- [x] **`hearsay-backends::reconcile_stranded_meetings`** — crash-recovery test (`tests/reconcile.rs`):
      a `recording` row with no `ended_at` is finalized (`ended_at` stamped) and its transcript.md /
      meeting.json written on the startup sweep; the empty case is a clean no-op.
- [ ] **`hearsay-inference` pure buffering** — the code says `StreamingSession::feed`/`finish` is FFI
      on the first line (sherpa recognizer), so there is no model-free seam there. The genuinely pure
      buffering — timestamp gap-fill / resync clamp — lives in `hearsay-orchestrator/src/recorder.rs`
      and `audio.rs`, which already have inline `#[cfg(test)]` tests. Retarget or drop this item.
- [ ] **`hearsay-db/queries.rs`** — direct unit tests for query paths and error branches not hit by
      `schema.rs` (decide per-query; do not duplicate integration coverage).
- [x] **Tauri shell (`web/src-tauri/src/main.rs`)** — `#[cfg(test)]` for `html_escape` (untrusted
      `detail` interpolated into a `win.eval` string — XSS-adjacent); `make tauri-test` added and wired
      into `make ci`. **Deferred:** `stop_core_gracefully` / `erase_all_data` (need `CommandChild` /
      `AppHandle` fakes).
- [x] **Parity: make the shared suite green on Windows.** `cfg`-gated
      `permissions_probe_degrades_when_helper_missing` to macOS (Windows has no helper). **Deferred:**
      diagnose `storage_override_pins_meeting_dir_off_the_default_root` (a possible Windows path bug,
      not a test bug — needs the Windows box) — both flagged in windows-refine-crash.md §4.
- [ ] **Windows capture logic (`hearsay-capture/src/wasapi_source.rs`)** — unit-test the pure-logic
      parts: `HEARSAY_WIN_LOOPBACK` device/process selection, the self-specified-format fallback,
      and gap-fill-by-timestamp. Real COM/audio devices stay manual smoke.
- [ ] **Windows backend wiring (`hearsay-backends/src/windows.rs`)** — `build_engine` assembly under
      the `sherpa` feature, mirroring the mac `build_engine` test.

## Phase 3 — Regression hardening

Lock behavior so future changes cannot silently regress it.

- [x] **Snapshot tests (`insta`)** for the markdown export — `render_transcript` (speaker grouping +
      `HH:MM:SS`) is snapshotted in `hearsay-orchestrator/src/markdown.rs`. Notes rendering already has
      assertion tests; an OpenAPI-doc snapshot is still open.
- [ ] **Migration upgrade tests** — seed a populated fixture DB at an older schema version, run the
      `Migrator`, assert a clean upgrade with data intact. Guards the forward-only/append-only rule
      (today every test starts from an empty DB).
- [x] **Property tests (`proptest`)** — IPC codec header+payload round trip in `hearsay-ipc`; the
      voiceprint `cosine` symmetry / bounds / self-similarity in `hearsay-attribution` (`match_identity`
      threshold selection stays covered by the existing unit tests).
- [ ] **Coverage measurement** — `cargo-llvm-cov` (Rust) + vitest v8 (web), report-only at first
      (baseline recorded here), ratcheted later.
- [ ] **Baselined model probes (`make probes`, both OSes)** — give the `#[ignore]`d probes recorded
      expectations so accuracy regressions surface. macOS: pin the refine anti-loop entropy thresholds
      (the whisper repetition-attractor loops). Windows: the refine-crash guards already in
      `hearsay-inference/tests/embed_cap_probe.rs` (TitaNet's 122.88s / 12288-frame embed cap —
      asserting *absence of death*, since the failure aborts the process) plus `refine_probe.rs`;
      keep them in the probe set so the fixed crash cannot silently return.
- [ ] **Swift pure-logic parity** — extend the CLT-friendly `selftest` harness to cover
      `SidecarIO` framing and the `Resampler` DSP (keeps the Command-Line-Tools build path; avoids
      the full-Xcode XCTest requirement). macOS-only (no Swift on Windows).

## Phase 4 — Full-stack integration & E2E

Two real end-to-end paths the current suite has neither of: the core exercised over its real socket,
and the UI driven in a real browser. Both run against a deterministic, model-free engine so they
assert exact output without the ANE/GPU.

**Enabling seam — a scripted engine mode for the core.** Today `api.rs` uses `DisabledEngine` (503s),
and `SYNTHETIC=1` feeds tone audio into the *real* sidecars (nondeterministic ASR). Add a model-free
engine that drives the real orchestrator pipeline + persistence but emits canned
transcript/status/resync events (reusing the scripted fakes in
`hearsay-orchestrator/src/testing.rs`), selectable via a test-only flag/env. Both harnesses below
share it, so a meeting produces identical, assertable output every run.

- [ ] **Scripted-engine mode** in `hearsay-core` — real HTTP + WS + on-disk DB + orchestrator
      pipeline, deterministic events, no models.
- [ ] **Full-stack HTTP/WS integration (Rust, in `make ci`).** Boot the real `hearsay-core` server on
      an ephemeral loopback port with the scripted engine and a real temp-file SQLite DB. Drive a full
      meeting over the wire with a real HTTP client + a real WS client (`tokio-tungstenite`):
      `start` → assert live `TranscriptEvent`/`StatusEvent` frames arrive over WS → `stop` → refine →
      read segments/speakers/notes back via REST. This is the real integration test — it exercises the
      TCP socket, the WS upgrade/broadcast, the orchestrator, and on-disk persistence together, none
      of which `api.rs` (oneshot, in-memory, `DisabledEngine`) touches.
- [ ] **Browser E2E (Playwright, `make e2e`, both OSes).** Run `hearsay-core` (scripted engine) + `vite dev`,
      then drive the real React app with Playwright through the full flow: load with the session token
      → start recording → watch the transcript populate over the live WS → stop → find the meeting in
      Library → rename a speaker → open the Notes panel, asserting the UI reflects each step.
- [ ] Fold the Rust full-stack test into `make ci`; add `make e2e` for the browser run (needs the
      Playwright browser binaries, so it stays its own on-demand target).
- [ ] Isolation & teardown per [Results & cleanup](#results--cleanup): tempdir output + an RAII
      process-group kill in both harnesses; Playwright `webServer` start/stop; reports under
      `outputs/e2e/`.
- [ ] **Windows packaged-app E2E (parity gain).** WebView2 exposes Edge WebDriver, so the shipping
      NSIS app *can* be driven with `tauri-driver` + WebdriverIO — the same
      record → transcript → stop flow, through the real packaged shell that macOS cannot test.
      Windows-only; a distinct target so it never blocks the cross-platform browser E2E.

**Known gap — the packaged macOS app (macOS-only; Windows is covered by the bullet above).** Apple
ships no WebDriver for `WKWebView`, so the shipping
`.app` cannot be driven by the standard `tauri-driver`; the only options are CrabNebula's paid fork
or a third-party embedded-WebDriver plugin compiled into the app — neither worth adding to a
noncommercial shipping binary for tests. The Playwright path drives the identical web bundle against
the identical core, so the only slice left to manual smoke-testing is the real WKWebView runtime plus
the shell↔core boot handshake (whose Rust side is unit-tested in Phase 2). Recorded as out of scope,
not overlooked.

## Coverage by test type

How the phases map onto the categories requested.

| Type | Have | Adding |
|---|---|---|
| **Unit** | attribution, notes-prompt, IPC codec, inline orchestrator/core units | web client layer + hooks (P1); sherpa/streaming buffering, `reconcile`, queries, Tauri shell (P2); Swift `SidecarIO`/`Resampler` (P3) |
| **Component** | none | MSW-mocked React components (P1) |
| **Integration** | axum router (api.rs, oneshot/in-memory), DB schema, orchestrator lifecycle | focused WebSocket gating (P2); **full-stack HTTP/WS over a real socket with a real DB** (P4) |
| **E2E** | none | **browser Playwright: real UI → real core → real pipeline** (P4); packaged WKWebView app is a documented manual-smoke gap |
| **Regression** | golden IPC fixtures, codegen drift | `insta` snapshots, migration-upgrade fixtures, `proptest`, baselined model probes (P3) |

## Open decisions

- **Swift tests:** extend `selftest` (Command Line Tools only, matches the current convention) vs a
  real XCTest/swift-testing target (needs full Xcode). Default: extend `selftest` for pure logic.
- **`queries.rs`:** direct unit tests vs continued reliance on `schema.rs` integration — decide per
  query, favoring direct tests for error branches.
- **Coverage thresholds:** report-only first, ratchet once a baseline exists.
- **Probe granularity:** one `make probes` for all model tests vs. per-area targets
  (`probes-refine`, `probes-sherpa`, `probes-notes`) so a maintainer can run just the relevant one.
  Default: a single `make probes`, split later only if runtimes make it annoying.
- **E2E engine substrate:** a dedicated scripted-engine mode (deterministic, model-free — the
  default here) vs. driving the browser E2E against `SYNTHETIC=1` + the real sidecars (realistic but
  nondeterministic ASR, so assertions must avoid transcript text). Default: scripted engine, shared
  by the Rust full-stack test and Playwright.
- **Packaged macOS app E2E:** left as manual smoke (no first-party `WKWebView` WebDriver). Revisit
  only if a maintained, license-compatible embedded-WebDriver option appears or CrabNebula's fork is
  licensed.
- **Windows runner:** a `scripts\test-windows.ps1` mirror (default — matches the existing
  `build-windows.ps1`) vs. requiring GNU `make` under Git-Bash/MSYS on the Windows box. Default: the
  PowerShell mirror, so Windows needs no extra toolchain.
- **Windows packaged-app E2E:** worth building now vs. deferred. It is a real parity gain (macOS
  can't do it) but adds a `tauri-driver` + WebdriverIO stack; default is to land the cross-platform
  browser E2E first and add the Windows packaged run as a follow-up.
