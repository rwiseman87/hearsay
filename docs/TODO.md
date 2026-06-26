# hearsay — working plan & TODO

Durable, resumable tracker. Check items off as you go. Canonical design doc: the plan at
`~/.claude/plans/i-want-to-plan-keen-lake.md`. IPC contract: `shared/protocol/ipc.md`.
Conventions: `CLAUDE.md`.

## How to resume

**Status (2026-06-26):** Phases 0 + **Phase 1 MVP** (backend **and** React UI) are built and merged to
`main` (fast-forward; no git remote). Capture → VAD → ASR → DB + live `transcript.md` + loopback REST/WebSocket
work on a real call; the React UI (Vite 8 + React 19 + TanStack Query, OpenAPI→TS types, served by the core
with the session token injected) builds, typechecks, and serves. All local runtime data — recordings, the
SQLite DB, models, capture-debug — now lives under the repo's `outputs/` (gitignored). 86 tests; `make ci` +
`make web-ci` both green.

**Pick up here → the Phase 1 exit test (needs the user, in a browser):** build the UI (`cd web && npm run
build`), `uv run hearsay serve`, open the printed `?token=` URL, start a meeting on a real call, and confirm
finals appear in **timestamp order** within ~2–3 s (a cross-stream ordering bug was found during UI testing
and fixed — re-validate it on-device), the UI matches `transcript.md`, Me/Them are correct, and reopening a
past meeting loads its segments from the DB. Then Phase 1 is done → Phase 2 (diarization).

Docs: `README.md` + `docs/{architecture,pipeline,api,development}.md`. Design: the plan. IPC: `shared/protocol/ipc.md`.

```sh
make sync                            # venv + base deps (Python 3.14)
uv sync --extra asr                  # transcription stack (whisper.cpp + onnxruntime VAD; torch-free)
swift build --package-path helper    # build the capture helper
uv run hearsay fetch-models          # Silero VAD model (~2 MB)
make ci                              # ruff + mypy --strict + pytest + swift selftest + audit + licenses
cd web && npm ci && npm run build && cd ..   # build the React UI bundle (web/dist)
make web-ci                          # web gate: npm ci + OpenAPI→TS drift + tsc + vite build
uv run hearsay serve                 # loopback API + WS + the built UI (prints URL + ?token= link)
uv run hearsay live --model base --seconds 60   # real pipeline -> live transcripts (on-device validation)
```

- Python core: `src/hearsay/`  ·  Swift helper: `helper/`  ·  web UI: `web/` (Vite + React + TS).
- Scratch / local artifacts go in `outputs/` (gitignored), not `/tmp`.
- The session-scoped task list is ephemeral; **this file is the source of truth** for progress.

## Decisions locked (do not relitigate)

- **Architecture:** hybrid — thin Swift capture helper + Python core + local web UI in a WKWebView.
- **Persistence:** local-first **SQLite** via async **SQLAlchemy 2.0** + **Alembic** (no raw SQL;
  models in `src/hearsay/models/`, migrations in `src/hearsay/db/migrations/`). Portable to Postgres later.
- **Python 3.14** (locked; full ML stack verified on cp314). Fallback ladder 3.13 → 3.12 only if a dep regresses.
- **ASR default:** whisper.cpp `large-v3-turbo` via `pywhispercpp` (Metal+CoreML), behind an `ASRBackend` protocol.
- **ASR backends (2026-06-26):** don't phase-split — ship **both** behind `ASRBackend`, gated by install extra:
  whisper.cpp default (`asr` extra, torch-free) + **mlx opt-in** (`accel` extra; mlx-whisper pulls torch). Models are
  **swappable at runtime** via config / `PUT /api/asr/model` (next-meeting effect). pywhispercpp is the only torch-free ASR.
- **VAD (2026-06-26):** **Silero via `onnxruntime`** (torch-free; the `silero-vad` pip pkg hard-depends on torch).
  Behind a `VAD` protocol + a pure `Segmenter` (hysteresis, partial/final). Bug found+fixed: Silero needs 64 samples of
  left-context per 512-frame. Model is pinned + sha256-checked, fetched by `hearsay fetch-models`.
- **Dependency policy (2026-06-26):** vet every new dep **live** (latest version, no CVEs, permissive license MIT/BSD/
  Apache) before adding. Verified clean: pywhispercpp 1.5.0, onnxruntime 1.27.0, numpy 2.4.6, silero/mlx/pyannote.
- **Diarization:** `pyannote.audio` 4.0 community-1, rolling window over the **Them** stream only.
- **LLM:** OpenAI-compatible client (Ollama/LM Studio/llama.cpp) by default; Bedrock Converse configurable.
- **Speaker ID layers:** channel (Me/Them) + calendar roster + live diarization + manual labeling w/ memory
  + active-speaker. **Active-speaker = OCR-primary** (ScreenCaptureKit+Vision), Zoom Accessibility opt-in.
- **Distribution:** notarized direct download, not sandboxed; **minimize scary perms** (Mic + Audio Capture +
  Screen Recording; Accessibility only for the opt-in Zoom AX path). Packaging deferred (Phase 5).
- **Output:** per-meeting folder with live `transcript.md` + LLM `notes.md` (separate files), via a `Sink` seam.
- **Conventions (CLAUDE.md):** uv, `mypy --strict`, StrEnum, thin routers + `services/`, Pydantic in `schemas/`,
  list endpoints return `{total,page,page_size,items}`, native fetch (no Axios), pinned lockfiles,
  permissive licenses only (MIT/BSD/Apache), `pip-audit` clean, npm `ignore-scripts=true`, Makefile task runner.

## Progress log

- **2026-06-26** **Phase 1 committed + merged to `main`.** The React UI, the cross-stream timestamp fix,
  and the local `outputs/` storage migration (entries below) landed on `feat/phase-1-mvp` and were
  fast-forwarded onto `main`; `make ci` green, working tree clean, no git remote. Phase 1 is
  feature-complete — only the in-browser exit test remains before Phase 2 (diarization).
- **2026-06-26** **Local-first storage moved under `outputs/`.** Runtime data now lives in the repo
  (gitignored), not `~/Documents` / `~/Library/Application Support`: `outputs/recordings` (per-meeting
  `transcript.md` + `meeting.json`), `outputs/db/hearsay.db`, `outputs/models`, and `outputs/capture-debug`
  (diagnostic `me.wav`/`them.wav`, previously CWD-relative). Settings: dropped `app_support_dir`;
  `output_dir`/`models_dir`/`database_url` + new `capture_debug_dir` all derive from one `_OUTPUTS_DIR` (still
  env-overridable). Migrated existing data in place (DB = 1 meeting/11 segments, 1.7G models — no re-download).
  `.gitignore` restructured: `outputs/*` ignored, the four dirs kept as structure via `.gitkeep` with data
  contents ignored (verified `git add` stages only the `.gitkeep`s); pruned stale `output/`/`capture-debug/`
  lines (kept `*.wav`). Tests + docs updated; `make ci` green.
- **2026-06-26** **Cross-stream transcript ordering bug found + fixed (surfaced by the UI).** The live UI showed
  every "Them" line above every "Me" line regardless of time. Root cause: `Segmenter` timed utterances by
  cumulative sample count anchored only at the first chunk, ignoring each chunk's `host_ts` — so system audio
  (silence ⇒ fewer delivered samples) drifted ~100 s behind the continuous mic, putting the streams on different
  timelines (violating the "align by timestamp, never by sample index" guardrail). Fix: `Segmenter.push`
  re-anchors the frame clock to each chunk's `host_ts` (offset by the buffered remainder) so both streams stay on
  the shared epoch. +1 regression test (cross-chunk re-anchor; the old tests only did single pushes) → 86 pass;
  added a per-stream first-chunk diagnostic log. Python-only (restart `serve`); existing DB rows keep their old
  timestamps, so re-validate on a fresh on-device recording.
- **2026-06-26** **Phase 1 increment 6 (React UI) built — Phase 1 feature-complete pending the browser exit test.**
  `web/` = Vite 8 + React 19 + TS strict + TanStack Query 5. One canonical typed fetch wrapper (`api/client.ts`,
  native fetch, `ApiError` from the `{detail}` envelope, `AbortSignal.timeout`) carries the per-session bearer token;
  typed query-key factory; client-level `QueryCache`/`MutationCache` error handlers. Types are codegen'd from the API:
  `scripts/dump_openapi.py` writes a deterministic `web/openapi.json`, `openapi-typescript` → `web/src/api/schema.ts`
  (`TranscriptEvent` is hand-mirrored — the WS isn't in OpenAPI). Single page: start/stop/delete a meeting + ASR model
  picker; `useTranscript` opens the WS and merges DB finals with live partial/final events keyed by `(stream,start_s)`,
  replacing each stream's partial with its next final (Me/Them colored). The core serves the built bundle
  (`api/web.py`): `/assets` via `StaticFiles`, `GET /` injects the token as `window.__HEARSAY_TOKEN__` behind a
  per-response CSP nonce + security headers; optional (API-only if `web/dist` absent). Token handoff: injected into the
  served HTML, `?token=` fallback in dev (Vite proxies `/api`+`/ws` to `:8137`). Decisions made live with the user:
  **one `.npmrc`, moved into `web/`** (npm's local prefix = the nearest dir with package.json, so the repo-root
  `.npmrc` is bypassed inside `web/` — verified); **package.json stays in `web/`** (mirrors `helper/`; the root is the
  Python project); **TypeScript pinned to 5.9.3, not latest 6.0.3** (openapi-typescript 7.13 peer-requires `^5.x`; no
  `--force`). Makefile: `codegen` extended + `web-install`/`web-typecheck`/`web-build`/`web-codegen-check`/`web-ci`; CI
  gains a `web` job (setup-node + `make web-ci`). 4 web-serving tests (token injection, per-response nonce, asset
  serving, API-only fallback) → **85 pass**; `make ci` + `make web-ci` green; smoke-served the real bundle (token
  injected, nonce matches the CSP, cross-site Origin on `/` → 403, API still 401 without a token). Uncommitted on
  `feat/phase-1-mvp`. **Remaining: the on-device browser exit test (start a meeting in the served UI on a real call).**
- **2026-06-26** **Docs written + Phase 1 backend MVP committed.** Refreshed `README.md` (status, quickstart,
  layout, docs index) and added `docs/architecture.md` (3-process design + module-by-module tour of the Python
  core — what each piece does + why + the seams table), `docs/pipeline.md` (the capture->VAD->ASR->DB+md+WS data
  flow with mermaid + rationale), `docs/api.md` (auth model + REST/WS reference with examples), `docs/development.md`
  (setup, make targets, serve/live, model management, testing, troubleshooting). Verified the nested-env config
  claims (`HEARSAY_ASR__MODEL` etc.) actually parse. `.gitignore`: added `outputs/` (scratch) + `.vscode/`.
  Increments 1-5 committed on `feat/phase-1-mvp`. Remaining Phase 1: React UI + OpenAPI->TS codegen (increment 6).
- **2026-06-26** **ON-DEVICE REAL-CALL VALIDATION PASSED + 3 fixes it surfaced.** User ran `hearsay live --model base`
  during a real call: **channel separation confirmed** (their voice -> `Me`, video audio -> `Them` — the make-or-break
  holds through the full pipeline), real-time finals in ~1-2s (Metal inference 0.03-0.37s), `transcript.md` +
  `meeting.json` written. Fixed what it exposed: (1) **transcript ordering** — Me/Them are independent consumers so
  finals appended in ASR-completion order, not time order; implemented the plan's **atomic finalize rewrite** (sink
  `close`->`finalize(lines)`, pipeline pulls all segments from the DB sorted by `start_s` and rewrites `transcript.md`
  grouped/in-order via temp+`os.replace`). (2) **non-speech tokens** — `_clean_text` drops clips whisper renders as a
  lone `[BLANK_AUDIO]`/`[Music]`/`(buzzing)`. (3) **log spam** — `WhisperCppBackend` now sets
  `redirect_whispercpp_logs_to=None` + `print_progress=False` + quiets the `pywhispercpp` logger (dozens of lines/
  utterance -> ~1). Re-verified: JFK still transcribes correctly. +2 tests (finalize ordering, `_clean_text`) -> 81
  pass; ruff + `mypy --strict` clean (incl. against pywhispercpp's real stubs now the extra is installed). Phase 1
  backend is **validated + polished**. Remaining: React UI (increment 6). Still uncommitted on `main`.
- **2026-06-26** **Real-backend env set up + `hearsay live` validation harness ready (UI deferred per user — validate first).**
  `uv sync --extra asr` resolves on cp314 (onnxruntime 1.27.0, pywhispercpp 1.5.0, numpy 2.4.6); `pip-audit` clean;
  license gate clean. `hearsay fetch-models` downloaded Silero. With the extra installed, the 2 Silero tests now run
  in-suite (download JFK, assert speech detection) → **80 pass**; and mypy now type-checks the whisper wrapper against
  pywhispercpp's real stubs (fixed: pass `language=` explicitly instead of `**params`). Added `hearsay live
  [--model M] [--synthetic] [--seconds N]` — runs the **exact** production path (SessionManager -> real HelperCapture
  -> TranscriptionPipeline -> whisper.cpp + Silero -> DB + transcript.md) and prints live `[final]`/`[partial]` lines.
  **Synthetic glue smoke passed**: helper spawned, base model downloaded+loaded on Metal, meeting folder +
  transcript.md created, clean stop (tones -> no speech -> empty transcript, as expected). `base` model now cached.
  **Next: user runs `hearsay live` in a real call (Phase 1 exit: Me=your voice, Them=remote, finals in transcript.md),
  then build the React UI (increment 6).**
- **2026-06-26** **Phase 1 increments 3 + 5 (transcript sink + pipeline wiring) done — backend MVP complete.**
  Increment 3: `export/` Sink seam (`TranscriptSink` protocol, `MeetingMeta`/`TranscriptLine`) + `LocalMarkdownSink`
  (single writer, complete newline-terminated blocks + `os.fsync`, consecutive same-speaker grouping under
  `### HH:MM:SS — Speaker`, atomic `meeting.json` via temp+`os.replace`); 4 tests. Increment 5: `transcript/pipeline.py`
  `TranscriptionPipeline` — per-stream consumer reads `AudioChunk`s, anchors both streams to the first `host_ts`,
  segments via VAD, transcribes off-loop (`to_thread` + lock), then **final** → DB (`add_segment`) + `transcript.md`
  + WS, **partial** → WS only. Wired into `MeetingSession`/`SessionManager` via injected `asr_factory`/`vad_factory`/
  `sink_factory` (real = whisper.cpp + Silero + LocalMarkdown; tests inject fakes); `Capture` now exposes `.media`;
  moved `Broadcaster` to `transcript/broadcast.py` (cycle break). Pipeline runs only when capture yields media, so the
  no-helper API tests still pass. Added ASR model picker: `GET /api/asr/models` + `PUT /api/asr/model` (swap-at-whim;
  next-meeting effect) + schemas. End-to-end pipeline test (fake media + stub VAD + fake ASR → real DB + sink +
  broadcaster) asserts the segment persists, `transcript.md` gets it, and a final WS event fires. 78 pass + 2 skip;
  ruff + `mypy --strict` clean. **Remaining Phase 1: React UI + OpenAPI→TS codegen (increment 6), then the on-device
  real-call exit test.** Uncommitted on `main`.
- **2026-06-26** **Phase 1 increment 4 (ASR + VAD) done — real backends verified on-device.** Decision (with the
  user): VAD = **Silero via onnxruntime** (torch-free; the `silero-vad` pip pkg drags torch+torchaudio, and
  mlx-whisper also pulls torch — pywhispercpp is the only torch-free ASR). Don't phase-split ASR: ship the
  `ASRBackend` protocol + **both** backends now, gated by install extra. Built: `vad/base.py` (`VAD` protocol +
  `Segmenter` with start/stop hysteresis + partial cadence — pure, 5 unit tests via a stub VAD), `vad/silero.py`
  (onnx inference; **bug found+fixed**: Silero needs 64 samples of left-context prepended per 512-frame or every
  frame scores ~0 — verified the fix on JFK: 234/343 speech frames, max 1.0; silence ~0), pinned+sha256 model
  download. `asr/base.py` (`ASRBackend` protocol + `ASRSegment`), `asr/whispercpp_backend.py` (pywhispercpp,
  default, torch-free), `asr/mlx_backend.py` (opt-in, lazy), `asr/manager.py` (`build_asr` dispatch + `resolve_model`
  name→backend-id, verified mlx-community repo slugs exist, `available_models`). Settings `asr`/`vad` groups +
  `models_dir`; `ASRBackendKind` enum; `hearsay fetch-models` CLI; `asr` extra now pywhispercpp+onnxruntime+numpy
  (torch-free). **On-device proof (M4 Max, cp314):** pywhispercpp loaded ggml-tiny on Metal and transcribed JFK
  correctly via `WhisperCppBackend`; Silero+Segmenter produced 4 clean utterances at JFK's pauses. 5 segmenter +
  4 manager tests (CI) + 2 guarded Silero tests (skip without onnxruntime) → 71 pass + 2 skip; ruff + `mypy --strict`
  clean. Uncommitted on `main`.
- **2026-06-26** **Phase 1 increment 2 (core skeleton + API) done.** `schemas/` (PEP 695 generic `Page`,
  `MeetingCreate`/`MeetingRead`, `SegmentRead`, `TranscriptEvent`); `services/MeetingService` (CRUD + pagination +
  finalize + cascade delete, folder-name slug helper). FastAPI app (`api/app.py` `create_app`): per-session bearer
  token + Host/Origin loopback allowlist (`api/security.py`), Annotated DI (`api/deps.py`), thin meetings router
  (`/api/meetings` CRUD + `/stop` + `/segments`), live `/ws/meetings/{id}` WebSocket (`api/ws.py`, token via
  `?token=`, Origin-checked, subscribes to the session broadcaster). Orchestration in `transcript/`: `Capture`
  protocol + `HelperCapture` (wraps the Phase-0 supervisor; media/ASR pipeline attaches here later),
  `MeetingSession` + `SessionManager` (one active meeting, lock-guarded start/stop/delete, creates the meeting row +
  folder), `Broadcaster` fan-out. `hearsay serve` CLI (auto-free-port, prints token URL, uvicorn on 127.0.0.1).
  16 new tests (TestClient: auth/host/origin, lifecycle, conflict 409, delete, WS auth+close codes; async
  broadcaster) → 62 pass; ruff + `mypy --strict` clean. **Smoke-tested the real server**: boots, 401 without token,
  200 paginated list with token, 400 on non-loopback Host. Uncommitted on `main`.
- **2026-06-26** **Phase 1 increment 1 (DB foundation) done.** Async SQLAlchemy 2.0 layer: `db/engine.py`
  (`create_engine` with SQLite WAL + `busy_timeout=5000` + `foreign_keys=ON` via a connect listener, explicit
  pool size/overflow/timeout/pre-ping), `db/session.py` (`create_sessionmaker`, `expire_on_commit=False`), and a
  `Database` holder (`db/__init__.py`) bundling engine+sessionmaker for DI/tests. Models: `models/base.py` (UUID PK
  + `created_at`/`updated_at` on the declarative `Base`; portable `str_enum()` = `VARCHAR`+CHECK storing StrEnum
  *values*), `Meeting` + `Segment` (FK `ON DELETE CASCADE`, composite index `ix_segments_meeting_start`,
  relationship `order_by=start_s`). Alembic wired (async `env.py`, `render_as_batch` for SQLite; `alembic.ini` with
  URL supplied from Settings at runtime); autogenerated frozen baseline `7e2c0680d390`; `alembic check` reports zero
  drift vs the models. Added `MeetingStatus` StrEnum. `tests/conftest.py` gives SAVEPOINT-isolated async sessions
  (external transaction + `join_transaction_mode="create_savepoint"`, plus the SQLite `isolation_level=None` +
  manual `BEGIN` listeners required to make pysqlite respect it). 5 new tests (round-trip, ordering, enum-values,
  DB-level cascade, migration runner) → 34 pass; ruff + `mypy --strict` clean. Migrations excluded from mypy/ruff
  (generated code). Uncommitted on `main`.
- **2026-06-26** **PHASE 0 COMPLETE.** On-device recovery re-test passed: a stress run with ~8 forced output device/
  rate changes saw both streams recover every time (`tap_health: recovered` ×many, `mic_health: recovered` ×3); "Me"
  captured the full 60 s (was dying at 21 s before the mic watchdog). All three exit criteria met — separation, drift
  (50 ms/60 s, 4.1 ms skew), and dual-stream recovery. The capture spike is trustworthy; next is Phase 1 (core skeleton,
  DB, VAD + whisper.cpp ASR, live `transcript.md`, minimal UI).
- **2026-06-26** Task 7 finished: Python capture-debug reader (`src/hearsay/helper/`): `control.py` NDJSON codec,
  `control_channel.py`/`media_channel.py` async channels (reply correlation + per-stream queues + seq-drop counting),
  `supervisor.py` (listen-both-sockets + spawn + await `hello` + graceful stop), `capture_debug.py` + `hearsay
  capture-debug` CLI (writes `me.wav`/`them.wav`, prints samples/RMS/drops). 29 pytest tests incl. a real-binary
  `--synthetic` integration test (CI builds the helper first); `make ci`-relevant gates green (ruff + mypy --strict +
  pytest + swift selftest). The whole Python↔Swift pipe verified off-device end-to-end (both tones, RMS 0.141, 0 drops,
  clean teardown). Bug fixed: the discarded media `StreamWriter` left the connection open, hanging
  `asyncio.Server.wait_closed()` at teardown — supervisor now owns/closes it (+ bounded wait). Phase 0 code-complete;
  **only the on-device capture-truth test remains (needs the user: TCC grant + real call).**
- **2026-06-26** On-device first-run gotcha (found during the user's first real run): the first `start_capture` blocks
  the helper while macOS shows the Microphone / System Audio Recording TCC prompts, which exceeded the 5 s reply
  timeout → core closed the sockets → helper writes hit `EPIPE (writeFailed(32))`. Fixed: `capture-debug` gives
  `start_capture` a 120 s timeout, prints an "accept the prompts" notice on the real path, and reports a clean message
  instead of a traceback. (Grants persist for the ad-hoc-signed binary until the next `swift build` changes its cdhash.)
- **2026-06-26** **On-device separation confirmed** (Phase 0's make-or-break): real run produced `me.wav` = user's
  voice, `them.wav` = system audio only. Added capture-debug instrumentation for the remaining drift / tap-recovery
  checks: drift columns (`audio_s` vs `host_ts`-derived `wall_s`, inter-stream start skew) and surfaced
  `status`/`tap_health`/`error` events. Also made `SyntheticSource` produce at real wall-clock rate (was `Thread.sleep`-
  paced, ~14% slow, which made the new drift metric lie in synthetic mode); now `audio_s ≈ wall_s`. All gates green.
- **2026-06-26** On-device drift test PASSED (skew 4.1 ms, ~50 ms/60 s, 0 drops). The tap-recovery test confirmed the
  **system-audio watchdog recovers** (multiple `tap_health: recovered` on output-rate changes) but exposed a real bug:
  **the mic died at ~21 s** — `AVAudioEngine` stops on an `AVAudioEngineConfigurationChange` and `MicCapture` never
  restarted it, so a device/rate change silently killed "Me". Fixed: `MicCapture` now observes the config-change
  notification and rebuilds (new input format → new resampler, reinstall tap, restart engine) on a serial queue under
  its lock — the mic-side counterpart to the tap watchdog — emitting a new `mic_health` event (added to `ipc.md` +
  surfaced in capture-debug). Swift build clean, `make test` green. Needs an on-device re-run to confirm mic recovery.
- **2026-06-26** Task 6 finished: `serve` orchestrator wired (`Serve.swift`) + `main serve` dispatch + embedded
  `Info.plist`. `swift build` clean (zero warnings), `make test` green. Whole IPC pipe verified off-device with a
  Python harness speaking the real `protocol.py` codec against `serve --synthetic`. Decisions: **wire audio as
  float32** (capture graph is already 16 kHz mono `Float`, so payload is a zero-cost lossless reinterpret; Python
  reader will convert to int16 for WAV); **`host_ts` stamped at drain time, backlog-corrected** (`payload[0]` =
  oldest queued sample, so `now − backlog/16kHz`) + per-stream monotonic clamp — keeps both streams aligned and
  `host_ts` strictly increasing even when a tick emits several frames; later-phase commands reply a structured
  `unsupported` error rather than hanging. **Remaining in Phase 0: all of Task 7 (Python reader), then the
  on-device capture-truth exit test.**
- **2026-06-25** Phase 0 foundation landed: docs reconciled, Python 3.14 locked, scaffold + tooling,
  cross-language IPC `FrameCodec` (golden-fixture verified both ways), Makefile + CI + license/CVE gates green.
- **2026-06-25** Task 6 capture stack (Swift) mostly landed, `swift build` clean (zero warnings):
  HearsayIPC control NDJSON types (`JSONValue`/`Command`/`Reply`/`Event` + `ControlCodec`) + UDS transport
  (`UnixSocketClient`/`LineReader`); capture utils (`Clock` monotonic `host_ts`, lock-guarded `RingBuffer`,
  `Resampler` -> 16k mono, `SyntheticSource` test tones); real capture (`MicCapture` AVAudioEngine,
  `SystemAudioTap` global-except-self process tap + aggregate device + IOProc + device/sample-rate watchdog,
  `Permissions`). Verified every native API against the SDK headers + a typecheck spike before coding.
  Decisions: hand-rolled the helper CLI (no `swift-argument-parser`); `serve --synthetic` streams tones so the
  whole IPC pipe is testable without TCC/audio hardware; executable uses Swift 5 language mode (RT-audio
  closures), HearsayIPC stays strict Swift 6. **Remaining: `Serve` orchestrator + `main serve` dispatch +
  Info.plist (Task 6), then all of Task 7 (Python), then the on-device exit test.**

---

## Phase 0 — Skeleton, capture spike, permissions

Done:
- [x] Reconcile `CLAUDE.md` + plan with locked decisions (SQLite+SQLAlchemy+Alembic, Python 3.14).
- [x] Lock Python 3.14 (verified full ML stack resolves on cp314).
- [x] IPC contract spec — `shared/protocol/ipc.md` (28-byte media frame + NDJSON control).
- [x] Python project scaffold + tooling (pyproject, mypy/ruff/pytest, package skeleton, logger, settings, enums).
- [x] Python `FrameCodec` + golden fixtures + tests — `src/hearsay/helper/protocol.py`, `shared/fixtures/frames.jsonl`.
- [x] Swift helper SwiftPM package + `FrameCodec` mirror + `selftest` (decodes/re-encodes the committed fixtures).
- [x] Makefile + license/CVE gate + CI workflow (`make ci` green).

Next — **Task 6: Swift audio capture + IPC streaming** (`helper/`):
- [x] ~~Add `swift-argument-parser`~~ → hand-rolled CLI instead (3 subcommands; keep helper dependency-free).
- [x] `HearsayIPC`: NDJSON control types (`JSONValue`/`Command`/`Reply`/`Event` + `ControlCodec`) + UDS
      transport (`UnixSocketClient` + `LineReader`). Mirrors `ipc.md`. (Higher-level connect/hello lives in `serve`.)
- [x] `Audio/SystemAudioTap.swift` — `CATapDescription(monoGlobalTapButExcludeProcesses:)` +
      `AudioHardwareCreateProcessTap` + private aggregate device (`tapautostart`+drift) + IOProc → ring buffer.
- [x] `Audio/MicCapture.swift` — `AVAudioEngine.installTap` → ring buffer ("Me").
- [x] `Audio/Resampler.swift` (`AVAudioConverter` → 16 kHz mono Float32) + `Clock.swift`
      (`clock_gettime_nsec_np(CLOCK_UPTIME_RAW)` monotonic `host_ts`).
- [x] `RingBuffer.swift` — SPSC ring per stream (NSLock-guarded, not lock-free; overruns counted). Uplink in `serve`.
- [x] Zero-buffer watchdog — property listeners (default-output-device + nominal sample rate) rebuild **both**
      tap and aggregate and emit `tap_health: recovered`; silence timer emits `zero_buffers` telemetry + one
      start-time rebuild. (Heuristic; tune the silence path on-device — real silence must not thrash rebuilds.)
- [x] `Permissions.swift` — Microphone status/request via `AVCaptureDevice`; `snapshot()` for `check_permissions`
      (audio_capture confirmed when the tap builds; screen/AX/calendar are later phases).
- [x] Wire `serve` — `Serve.swift` (connects both sockets, emits `hello`; uplink thread drains rings →
      **float32** framed PCM on `media.sock` with backlog-corrected monotonic `host_ts`; handles `ping`/
      `check_permissions`/`start_capture`/`stop_capture`/`shutdown` + `unsupported` for later-phase cmds; emits
      `status`/`tap_health`/`level`/heartbeats; SIGTERM/SIGINT graceful exit, SIGPIPE-safe) + `main.swift`
      `serve --socket-dir DIR [--synthetic]` dispatch + control round-trip + golden-wire checks in `selftest`.
      **Verified end-to-end off-device:** Python harness (real `protocol.py` decoder) drove the synthetic pipe —
      hello/ping/perms/start/stop/shutdown all correct; per stream one hello(seq 0)+audio+one eos, seq & host_ts
      strictly increasing, tone RMS 0.141 (=0.2/√2), stream skew 0 ms, clean exit.
- [x] `Info.plist` usage strings (`NSMicrophoneUsageDescription`, `NSAudioCaptureUsageDescription`) embedded via
      linker `-sectcreate __TEXT __info_plist` (absolute path from Package.swift `#filePath`); verified in the
      Mach-O section. TCC attribution for the bare executable is unverified until the on-device test (full bundle
      attribution is Phase 5).
- [x] `swift build` clean (zero warnings).

Done — **Task 7: Python capture-debug reader** (`src/hearsay/helper/`):
- [x] `control.py` — NDJSON `Command`/`Reply`/`Event` dataclasses + encode/parse (sorted-keys wire matches Swift).
- [x] `protocol.py` — `expected_payload_len()` (size the payload read from a header) + `audio_samples()` (int16/float32 → floats).
- [x] `control_channel.py` — async read loop: reply/command-id correlation (`call()`), event queue (`wait_for_event`), EOF fails pending.
- [x] `media_channel.py` — async frame pump → per-stream `AudioChunk` queues + `StreamStats` (seq-gap drop counting), `None` = EOS/EOF.
- [x] `supervisor.py` — run dir + bind/listen both sockets (`_OneShotServer`), spawn helper, await `hello`, graceful `stop()`.
      (Respawn-with-backoff deferred to the Phase 1 `MeetingSession`, per ipc.md step 5.)
- [x] CLI `hearsay capture-debug --seconds N --out DIR [--synthetic] [--helper PATH]` → drains both streams, writes
      `me.wav`/`them.wav` (stdlib `wave`, 16 kHz mono int16), prints samples/seconds/RMS/dropped. `helper_path` in Settings.
- [x] Tests: control codec + protocol helpers + channel round-trips over a socketpair, **plus a real-binary integration
      test** (`--synthetic`, skipif not built). `make test` builds the helper first so it runs in CI. **Verified:** the
      full Python↔Swift pipe streams both tones (RMS 0.141, 0 drops) and tears down cleanly.
      Fixed a teardown hang: the media `StreamWriter` was discarded, so `asyncio.Server.wait_closed()` blocked on the
      still-open connection — the supervisor now owns + closes it, and `wait_closed()` is bounded as insurance.

**Phase 0 exit — capture truth test (ON-DEVICE, needs the user):**
- [x] Grant TCC: Microphone + Audio Capture (prompted on first real `start_capture`, accepted).
- [x] Real run → `me.wav` = only your voice, `them.wav` = only system audio, both 16 kHz — **user confirmed separate**
      (mic vs YouTube). The global-except-self tap captures system audio cleanly, mic is "Me". Core spike proven.
- [x] Drift: 60 s real run → `me` audio_s 60.00 / wall_s 59.95, `them` 59.99 / 59.99, start skew 4.1 ms, 0 dropped.
      Sample-time tracks wall-clock within ~50 ms over a minute; far inside the 750 ms tolerance. PASS.
- [x] Recovery: stress run with ~8 output device/rate changes → **both** streams recovered every time
      (`tap_health: recovered` ×many, `mic_health: recovered` ×3). `me` captured the full 60 s (audio_s 57.77 / wall_s
      59.91) instead of stalling at 21 s; skew 9.4 ms, 0 dropped. Each stream loses <1 s per rebuild (inherent) but
      never goes silently dead. **Phase 0 exit test PASSED.**
- capture-debug tooling added for these: drift columns (`audio_s`/`wall_s` from `host_ts` + inter-stream skew),
  surfaced `status`/`tap_health`/`mic_health`/`error` events, 120 s `start_capture` timeout (first-run TCC prompts block).

---

## Phase 1 — MVP: capture → live transcript → markdown → minimal UI

- [x] Core skeleton: FastAPI app, settings DI, `MeetingSession`, supervisor, media/control channels.
- [x] DB: SQLAlchemy async engine + session, `meetings`/`segments` models (UUID PK + timestamps), Alembic baseline.
- [x] VAD segmentation (Silero via onnxruntime, torch-free) + sliding-window Segmenter (partial/final) per stream.
- [x] ASR backend `whispercpp` (`large-v3-turbo`, Metal+CoreML) behind `ASRBackend` protocol (+ opt-in `mlx` backend).
- [x] `transcript.md` live append (single writer, complete blocks, Me/Them by channel) + meeting folder + `meeting.json`.
- [x] WebSocket partial+final + loopback + per-session token (`/ws/meetings/{id}`; pipeline broadcasts
      `TranscriptEvent`s, Origin-checked, token via `?token=`); React UI consumes it (`useTranscript`).
- [x] OpenAPI → TS codegen wired (`scripts/dump_openapi.py` → `web/openapi.json` → `openapi-typescript`;
      `make codegen` + `make web-codegen-check`; the CI `web` job fails on drift).
- [ ] Verify (Phase 1 exit): finals in UI < ~2–3 s; UI matches `transcript.md`; Me/Them correct; reopening a past
      meeting loads segments from the DB. (Backend already validated on a real call; this is the UI half — needs a browser.)

**Increment 6 — React UI (`web/`) — DONE (build/typecheck/serve verified; browser exit test is the Phase 1 verify above):**
- [x] `web/` scaffold: Vite 8 + React 19 + TS strict; `web/.npmrc` (`ignore-scripts`+`save-exact`); pinned `package-lock.json`.
- [x] One canonical typed fetch wrapper carrying the per-session token (native fetch, no Axios; `ApiError` from the
      `{detail}` envelope, `AbortSignal.timeout`); TanStack Query + typed query-key factory + client-level
      (`QueryCache`/`MutationCache`) error handlers.
- [x] `openapi-typescript` codegen: `scripts/dump_openapi.py` → `web/openapi.json` → `web/src/api/schema.ts`; wired into
      `make codegen`; `make web-codegen-check` + the CI `web` job fail on drift.
- [x] Minimal single page: start/stop/delete a meeting; live transcript over the WS (`useTranscript` merges DB finals +
      WS events, keyed by `(stream, start_s)`, partial replaced by its final); ASR model picker (`GET`/`PUT /api/asr/model`).
- [x] Core serves the built bundle (`api/web.py`: `StaticFiles` for `/assets` + `GET /` injecting the token as
      `window.__HEARSAY_TOKEN__` behind a per-response nonce) with a minimal CSP + security headers (API-only if unbuilt).
- [x] Token delivery: served `index.html` gets the nonce'd inline token script; dev (Vite proxy) falls back to `?token=`.

## Phase 2 — Diarization (Them) + manual labeling + memory

- [ ] pyannote 4.0 community-1 rolling window over Them; cluster stabilization (embedding centroid → stable id).
- [ ] Fusion engine v1 (`fusion/`): channel + clusters → "Speaker N"; provisional/confidence.
- [ ] UI rename "Speaker N" → Identity; manual-override lock; cross-meeting `identities`/`speaker_memory`.
- [ ] Finalize: one atomic `transcript.md` rewrite with resolved names.
- [ ] Verify: stable Speaker 1..N for Them with Me separate; rename persists + is suggested next meeting.

## Phase 3 — Calendar roster + OCR active-speaker fusion

- [ ] Helper: EventKit roster → `roster` event; ScreenCaptureKit + Vision OCR worker → `name_hint` (throttled).
- [ ] Platform adapters (`platforms/`): zoom/teams/meet/slack window match + name-caption ROI.
- [ ] Fusion: attach hints to clusters (±0.75 s), roster-constrained `difflib` canonicalization, majority-vote binding.
- [ ] Confidence/provisional UI; graceful degrade (OCR off → Speaker N + manual).
- [ ] Verify: clusters auto-bind to correct roster names within minutes; one wrong hint never flips a stable binding.

## Phase 4 — LLM notes + Bedrock

- [ ] `LLMProvider` protocol; `openai_compatible` (Ollama/LM Studio) default + `bedrock` (Converse/ConverseStream, lazy boto3).
- [ ] Notes pipeline: rolling summary/decisions/action-items, delta-prompted, atomic `notes.md` rewrite; final pass at finalize.
- [ ] Settings: provider/model/base_url/region/notes cadence.
- [ ] Verify: `notes.md` updates without corruption; local↔Bedrock switch works; final notes capture decisions/action items.

## Phase 5 — Packaging + opt-in AX + post-meeting refine (DEFERRED until distribution)

- [ ] Swift WKWebView shell as bundle root; bundle relocatable CPython; depth-first codesign + hardened runtime + notarize + staple.
- [ ] First-run permission onboarding; opt-in Zoom `AXUIElement` worker → higher-confidence hints.
- [ ] Post-meeting full-file pyannote refine + notes regen; runbooks.
- [ ] Verify on a clean Mac: Gatekeeper passes; only expected permissions prompted.

---

## Risks / watch-items

- Core Audio tap **zero-buffer bug** → watchdog rebuilds tap + aggregate (Phase 0).
- **Teams per-process tap is silent** → default global-except-self tap.
- Mic/system **clock drift** → resample to 16 kHz, single `host_ts` clock, align by timestamp (±0.75 s).
- **OCR fragility** → roster-constrained matching + majority vote + graceful degrade.
- **Notarizing bundled Python** → hardest step; budget calendar time (Phase 5).
- **Compute contention** (whisper + pyannote + local LLM) → make in-meeting LLM optional; Bedrock to offload.

## Parking lot / open questions

- Voiceprint enrollment (ECAPA) — seam designed (`fusion/embeddings.py`), not built; revisit if recurring-team auto-labeling is wanted.
- Browser meeting apps (Meet/Slack) may warrant a browser extension later (more reliable than OCR/AX for web).
- Commit strategy: Phases 0–1 are committed and merged to `main`; no git remote yet (local-only).
