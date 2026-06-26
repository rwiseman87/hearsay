# hearsay — working plan & TODO

Durable, resumable tracker. Check items off as you go. Canonical design doc: the plan at
`~/.claude/plans/i-want-to-plan-keen-lake.md`. IPC contract: `shared/protocol/ipc.md`.
Conventions: `CLAUDE.md`.

## How to resume

```sh
make help     # list targets
make sync     # venv + deps (Python 3.14)
make ci       # ruff + mypy --strict + pytest + swift selftest + pip-audit + license gate
make test     # Python tests + Swift cross-language self-test
make codegen  # regenerate shared/fixtures from the codec
```

- Python core: `src/hearsay/`  ·  Swift helper: `helper/` (`swift build --package-path helper`)
- The session-scoped task list is ephemeral; **this file is the source of truth** for progress.

## Decisions locked (do not relitigate)

- **Architecture:** hybrid — thin Swift capture helper + Python core + local web UI in a WKWebView.
- **Persistence:** local-first **SQLite** via async **SQLAlchemy 2.0** + **Alembic** (no raw SQL;
  models in `src/hearsay/models/`, migrations in `src/hearsay/db/migrations/`). Portable to Postgres later.
- **Python 3.14** (locked; full ML stack verified on cp314). Fallback ladder 3.13 → 3.12 only if a dep regresses.
- **ASR default:** whisper.cpp `large-v3-turbo` via `pywhispercpp` (Metal+CoreML), behind an `ASRBackend` protocol.
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

- [ ] Core skeleton: FastAPI app, settings DI, `MeetingSession`, supervisor, media/control channels.
- [ ] DB: SQLAlchemy async engine + session, `meetings`/`segments` models (UUID PK + timestamps), Alembic baseline.
- [ ] VAD segmentation (Silero or whisper.cpp built-in) + sliding window per stream.
- [ ] ASR backend `whispercpp` (`large-v3-turbo`, Metal+CoreML) behind `ASRBackend` protocol.
- [ ] `transcript.md` live append (single writer, complete blocks, Me/Them by channel) + meeting folder + `meeting.json`.
- [ ] WebSocket: partial+final segments to a minimal React UI (start/stop, live transcript); loopback + per-session token.
- [ ] OpenAPI → TS codegen wired (`make codegen` extended); thin routers + `services/`.
- [ ] Verify: finals in UI < ~2–3 s; `transcript.md` matches UI; Me/Them correct; reopening a past meeting loads from DB.

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
- Commit strategy: foundation currently uncommitted on `main`; branch + commit when ready.
