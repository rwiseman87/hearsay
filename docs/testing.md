# Test suite

Hearsay's tests run **from the Makefile** — no timers, no git hooks. `make ci` also runs in GitHub
Actions on every push and pull request; everything else is on demand. Rust is the source of truth for
the API + IPC contract, and the tests consume that contract rather than re-implementing it. There are
two tiers:

- a **deterministic gate** (`make ci`) — fast, hardware-independent, model-free; the pre-flight before
  a commit or PR, and what CI enforces.
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
| `audit` | `cargo audit` (both Rust trees) + `npm audit` | no un-ignored advisories (the ignore list + rationale live in `web/src-tauri/.cargo/audit.toml`) | no |
| `licenses` | `cargo deny check licenses` (policy `rust/deny.toml`) | every dependency is MIT/BSD/Apache-2.0 | no |
| `web-ci` | `web-install` + `web-typecheck` (tsc) + `web-lint` (eslint) + `web-test` + `web-build` | the web UI type-checks, lints, unit/component-tests, and builds | no |
| `web-test` | `vitest` (jsdom) | the client layer + hooks + MSW-mocked components (also folded into `web-ci`) | no |
| **`make e2e`** | build the core, then Playwright/Chromium vs the scripted core + `vite dev` | the real React app → real core → real pipeline over a live WebSocket | no (Chromium once) |
| **`make probes`** | `cargo test -- --ignored` on `hearsay-notes` (metal) | the notes LLM | **yes** |
| **`make diarize-eval`** | `swift-build`, then the `diarization_accuracy` gate over the local labeled corpus | `hearsay-diarize` speaker-count + DER vs the committed baseline (AMI, plus any local recordings added to the corpus; audio stays local). The same test self-skips inside `make ci`; re-baseline an intentional change with `HEARSAY_UPDATE_DIAR_BASELINE=1` | **yes** |
| **`make wer-eval`** | `swift-build`, then the `hearsay-eval` `asr_accuracy` gate | the offline refine (`hearsay-diarize` with Parakeet Ultra) scored with WER and cpWER against the committed reference transcript, plus RTF in the run report; self-skips when the audio or sidecar is absent. Gated on Parakeet Ultra: WER 0.163 / cpWER 0.225 on AMI ES2004a near-field, 0.238 / 0.301 far-field. Re-baseline with `HEARSAY_UPDATE_EVAL_BASELINE=1` | **yes** |
| **`make diarizer-eval`** | `swift-build`, then the `hearsay-eval` `diarizer_compare` test (`HEARSAY_DIARIZER_EVAL=1`) | each candidate engine (`hearsay-diarize --diarizer <engine>`: pyannote, Nemotron 3, Sortformer, LS-EEND variants) over AMI ES2004a near-field and far-field: speaker count, DER with the missed / false-alarm / confusion split (collar 0.25 s), embedding count, wall time and RTF. Report-only (never gates); downloads each engine's models on first run; `HEARSAY_DIARIZER_ENGINES=a,b` restricts the engines. The report is written to `outputs/eval/<stamp>/diarizers.json` | **yes** |
| **`make live-eval`** | `swift-build`, then the `hearsay-eval` `live_eval` gate (`HEARSAY_LIVE_EVAL=1`) | `hearsay-me` and `hearsay-live` fed the corpus concurrently at real-time pace; WER and cpWER gated, final-delay percentiles reported. A 10-minute window takes about 10 minutes; `HEARSAY_EVAL_SPEED=0` feeds unpaced (WER only) | **yes** |
| **`make eval`** | `diarize-eval` + `wer-eval` + `live-eval` | every accuracy and latency eval | **yes** |
| `make coverage` | `cargo-llvm-cov` + vitest v8 → `outputs/coverage/` | report-only; the "what's untested" view | no |
| **`make test-all`** | `ci` + `probes` + `e2e` | everything, on a fully-equipped box | yes |
| `make clean-test` | `rm -rf outputs/coverage outputs/e2e` | (removes report dirs; test *data* auto-cleans via tempdirs) | no |

`make ci` + `make e2e` is the complete deterministic suite on a machine without the models.

## The suite by layer

| Layer | What's tested | Where | Run by |
|---|---|---|---|
| **Pure-logic units** | speaker ordering / segment-speaker assignment / voiceprint matching (`hearsay-attribution`); notes prompt build + reply parse (`hearsay-notes-prompt`); IPC frame + control codec (`hearsay-ipc`); inline core/orchestrator/inference units (AEC, recorder gap-fill, markdown render) | `#[cfg(test)]` mods in each crate's `src/` | `rust-test` |
| **Audio archival** | the FLAC encoder round-tripping bit-exactly at every block boundary, the decoder, the verifier rejecting truncated/corrupt/mismatched encodes, and `compress_meeting_audio` leaving the wav untouched on any failure | `hearsay-audio/src/` `#[cfg(test)]` mods | `rust-test` |
| **Archive container validity** | that STREAMINFO declares a coherent frame-size range and the true sample count. Asserted directly rather than via a round trip: decoders ignore those fields, so a bit-exact round trip passes over a file no macOS player will play (see docs/architecture.md) | `hearsay-audio/src/encode.rs` | `rust-test` |
| **Archival sweep** | which meetings are eligible (aged + finalized only, `ended_at` fallback), that it yields to a live meeting, skips already-archived and unrecorded meetings, and does not retry a failure | `hearsay-backends/tests/archive.rs` | `rust-test` |
| **Database** | schema round-trips over real migrations, `queries.rs` branches (FTS search, cross-meeting edit scope, the `reassign_segment_speaker` guards, cycle guard), the migration-upgrade fixture (populated old DB → head, data intact) | `hearsay-db/tests/schema.rs` (in-memory SQLite) | `rust-test` |
| **Core HTTP API** | the assembled axum router via `tower::oneshot` against in-memory SQLite with `DisabledEngine` — every REST route: meetings, segments (edit + per-line speaker reassign), speakers, folders, notes, search, settings, auth/Origin gating | `hearsay-core/tests/api.rs` | `rust-test` |
| **Full-stack HTTP/WS** | the core booted on a real TCP port with a scripted orchestrator: `start` → live `TranscriptEvent` frames over a real WS client → `stop` → REST read-back; a companion refine read-back replaces the Them track | `hearsay-core/tests/api.rs` | `rust-test` |
| **Orchestrator** | meeting lifecycle, the capture→transcribe→persist pipeline, recorder/markdown, refine — all with scripted fakes (fake audio source + stubbed transcribers/diarizer, no ML) | `hearsay-orchestrator/tests/`, `src/testing.rs` | `rust-test` |
| **Web client + hooks** | `api/client.ts` (request shaping, token, timeout, error envelope), `api/ws.ts` (reconnect/backoff, parse), the query hooks + `queryKeys`, `hooks/useTranscript.ts` (live assembly) against a stubbed fetch/WebSocket | `web/src/**/*.test.ts(x)` | `web-test` |
| **Web components** | `TranscriptView` (line edit + speaker reassign), `Library` (list/select/delete), `SettingsPage` (models validation, the storage archival policy + its full-section writes) against the real fetch wrapper + MSW | `web/src/components/*.test.tsx` | `web-test` |
| **Browser E2E** | real UI → real core (scripted engine) → real pipeline: start → live transcript → stop → Library → rename speaker → reassign a line → generate notes, asserting exact text; the storage panel round-tripping the archival policy; and a meeting whose audio exists only as FLAC serving, seeking, and playing in a real browser | `web/e2e/*.spec.ts` | `e2e` |
| **IPC parity** | the codec against golden fixtures, in **both** languages | `hearsay-ipc/tests/golden_fixtures.rs` + Swift `selftest` | `rust-test`, `swift-test` |
| **Regression locks** | `insta` snapshot of the markdown export; `proptest` for the IPC codec round-trip + voiceprint `cosine`; codegen-drift diff | across the crates above + `codegen-check` | `rust-test`, `codegen-check` |
| **Model/hardware probes** | the notes LLM (`summarize`) | `hearsay-notes/tests/` (`#[ignore]`d) | `probes` |

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
- **`memory_pool()`** (`hearsay_db::test_support`) — a single-connection `sqlite::memory:` pool
  running the real migrations, shared by every crate's tests. Single-connection because each
  connection to `sqlite::memory:` gets its own database.
- **`seg()` / `chunk()`** (`hearsay_orchestrator::testing`) — the segment and capture-chunk builders,
  alongside the scripted fakes that consume them.
- **MSW** — mocks the HTTP API for the React component tests while the real fetch wrapper runs.

## Isolation and artifacts

Every test that touches disk points `HEARSAY_OUTPUT_DIR` + `DATABASE_URL` at a `tempfile::tempdir()`
(or in-memory SQLite), so a run never writes the real `outputs/db/` or `outputs/recordings/` and leaves
the working tree untouched; `TempDir` cleans itself on drop (including on panic). The full-stack Rust
test serves the core in-process on an ephemeral port (no external process to reap); the browser E2E is
the one case that spawns a real process, managed by Playwright's `webServer` config. Triage artifacts —
coverage (`outputs/coverage/`) and Playwright reports/traces (`outputs/e2e/`) — land only under the
gitignored `outputs/`; `make clean-test` removes them.

## Running the probes

`make probes` is not model-free: the notes probe hard-requires a downloaded GGUF model (a bare
`make probes` fails by design). Point it at one:

```sh
HEARSAY_NOTES_MODEL=outputs/models/<instruct>.gguf \
make probes
```

The archival sweep compresses meetings under the dev output dir once they are old enough, so a corpus
recording may be `audio.flac` rather than `audio.wav`. The refine and `WavFileSource` read either;
point `HEARSAY_BENCH_WAV` at whichever the folder holds when running the evals.

## Accuracy and latency evals

`make eval` measures what the unit tests cannot: how accurate and how fast the shipped models are on
real speech. The metrics are pure Rust in `hearsay-attribution` (`word_errors`, `cpwer`, `percentiles`,
and `der`), and the runners live in `hearsay-eval` (plus the diarization gate in `hearsay-inference`).

| Eval | Measures | Scored against |
|---|---|---|
| `diarize-eval` | speaker count, DER | the AMI RTTM |
| `diarizer-eval` | per-engine speaker count, DER breakdown, RTF | the AMI RTTM |
| `wer-eval` | WER, cpWER, RTF of the refine | `shared/eval/ES2004a.utterances.json` |
| `live-eval` | live WER, cpWER, final delay | the same transcript |

- **Data.** The audio is local and never committed (`outputs/ami/`, fetched with the `curl` commands
  recorded in `shared/eval/corpus.json`). Committed: the manifest, the reference transcript, and the
  baselines `shared/eval/baseline-*.json`. The transcript is generated from the AMI public manual
  annotations (CC BY 4.0) by `uv run scripts/ami_words_to_json.py`. Score private recordings by
  pointing `HEARSAY_EVAL_CORPUS` at a manifest outside the repo.
- **Scoring.** Text is lowercased, punctuation and fillers (`uh`, `um`, `mm-hmm`) are dropped, digit
  strings are spelled out, and `ok` is unified with `okay`. WER compares all speakers merged in start
  order; cpWER matches each hypothesis speaker to the reference speaker that minimizes total errors, so
  a merged or split speaker is charged.
- **Gating.** Every gated metric is lower-is-better and may not exceed its baseline by more than 0.01
  (0.02 for live). A new reference or metric fails until baselined. A baseline records its window and is
  skipped, not failed, when a run used a different one (`HEARSAY_EVAL_MAX_S`,
  `HEARSAY_EVAL_LIVE_MAX_S`). Latency and RTF are machine-dependent, so they are reported in
  `outputs/eval/<run>/*.json` and not gated.
- **Comparing ASR backends.** `HEARSAY_EVAL_ASR=<v2|v3|ultra|redux|phonon2>` scores another
  `hearsay-diarize --asr` Parakeet model (each word attributed to the diarizer turn it overlaps)
  on the same metrics instead of the gated Ultra. Those runs are report-only: never gated, never
  written to the baseline. The first run of a model downloads it into the FluidAudio cache.

## Not covered (by design)

- **The packaged macOS `.app`** cannot be driven end-to-end: Apple ships no WebDriver for `WKWebView`.
  The Playwright path drives the identical web bundle against an identical core, so only the real
  WKWebView runtime + the shell↔core boot handshake are left to manual smoke (the Rust side of that
  handshake is unit-tested).
- **Real capture devices** (mic, system-audio tap) need TCC permissions and hardware; the pure
  device-selection/format/gap-fill logic is unit-tested, the live devices are manual smoke.
