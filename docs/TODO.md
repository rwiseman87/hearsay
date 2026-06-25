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

- **2026-06-25** Phase 0 foundation landed: docs reconciled, Python 3.14 locked, scaffold + tooling,
  cross-language IPC `FrameCodec` (golden-fixture verified both ways), Makefile + CI + license/CVE gates green.

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
- [ ] Add `swift-argument-parser` (Apache-2.0) and a `serve --socket-dir` subcommand.
- [ ] `HearsayIPC`: NDJSON control message types (Command/Reply/Event) + `ControlSocket` client (UDS) +
      `MediaSocket` client (send framed PCM). Keep mirror of `ipc.md`.
- [ ] `Audio/SystemAudioTap.swift` — `CATapDescription` (global-except-self) +
      `AudioHardwareCreateProcessTap` + `AudioHardwareCreateAggregateDevice`; IOProc → ring buffer.
- [ ] `Audio/MicCapture.swift` — `AVAudioEngine.installTap` → ring buffer ("Me").
- [ ] `Audio/Resampler.swift` + `Clock.swift` — `AVAudioConverter` to 16 kHz mono; single monotonic
      timebase stamping both streams (`host_ts`).
- [ ] `Audio/RingBuffer.swift` — lock-free SPSC ring per stream; uplink thread drains to `media.sock`.
- [ ] Zero-buffer watchdog — on sustained all-zero buffers, rebuild **both** tap and aggregate device;
      emit `tap_health`. Add Core Audio property listeners (default-output-device, nominal sample rate).
- [ ] `Permissions/Permissions.swift` — probe/request Microphone + Audio Capture; implement `check_permissions`.
- [ ] Wire `serve`: connect both sockets, send `hello`, handle `start_capture`/`stop_capture`/`shutdown`,
      emit `status`/`tap_health`/`level`/`permission` events.
- [ ] `Info.plist` usage strings (`NSMicrophoneUsageDescription`, `NSAudioCaptureUsageDescription`); decide
      TCC attribution for the spike (run as a bundled binary if attribution misbehaves).
- [ ] `swift build` clean.

Next — **Task 7: Python capture-debug reader** (`src/hearsay/helper/`):
- [ ] Extend `protocol.py` (or add `control.py`) with NDJSON Command/Reply/Event encode+parse.
- [ ] `supervisor.py` — create run dir, bind/listen on `media.sock` + `control.sock`, spawn helper, await `hello`, supervise/restart.
- [ ] `media_channel.py` — async reader: parse 28-byte header + payload → per-stream PCM queues (track `seq` drops).
- [ ] `control_channel.py` — NDJSON send/recv, command/reply correlation, typed event stream.
- [ ] CLI `hearsay capture-debug --seconds N --out DIR` — spawn helper, `start_capture`, drain both streams,
      write `me.wav` + `them.wav` (stdlib `wave`), print durations + RMS.

**Phase 0 exit — capture truth test (ON-DEVICE, needs the user):**
- [ ] Grant TCC: Microphone + Audio Capture.
- [ ] 2-min real call → `me.wav` = only your voice, `them.wav` = only the others, both 16 kHz, drift within tolerance.
- [ ] 15-min run; force a 44.1 kHz playback to confirm the tap auto-recovers (`tap_health: recovered`).

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
