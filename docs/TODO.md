# hearsay — working plan & TODO

Durable, resumable tracker. Check items off as you go. Canonical design doc: the plan at
`~/.claude/plans/i-want-to-plan-keen-lake.md`. IPC contract: `shared/protocol/ipc.md`.
Conventions: `CLAUDE.md`.

## How to resume

**Status (2026-06-30):** Phases 0 + **Phase 1 MVP** complete (merged to `main`, validated on a real meeting).
**Phase 2 (diarization) is CODE-COMPLETE and merged to `main`** (2026-06-30, fast-forward `5e8c74e..156944b`; Inc 1–7d
+ cross-meeting voiceprint recognition). **Diarization default flipped (2026-06-30): pyannote
`speaker-diarization-community-1` post-meeting refine is now the default** (`diarization.refine` defaults on, records
`them.wav`); the torch-free online clusterer is the live labeler + bundle-able fallback (torch-free rescoped to a
deferred Phase-5/distribution concern). **ASR default → `large-v3`** (was turbo; chosen by an on-device A/B). Built:
identities/clusters/
`segments.cluster_id` + `SpeakerService`; torch-free `OnnxSpeakerEmbedder` (wespeaker CAM++_LM); pure-stdlib
`OnlineSpeakerClusterer`; `MeetingDiarizer` in the pipeline; `GET/PUT /speakers` rename (binds + locks + retroactively
relabels) + React `SpeakerPanel`; finalize bakes resolved names into `transcript.md`; `ThemAudioRecorder` (records
`them.wav` only when refine is on); `PyannoteDiarizer` + `rediarize_meeting` + `hearsay rediarize <id|latest>` +
`POST /meetings/{id}/rediarize` + "Refine speakers" UI button; cross-meeting voiceprint auto-recognition (a returning,
manually-locked speaker is auto-named provisionally). **139 tests; full `make ci` + web tsc/build/codegen green.**

**Pick up here → the open problem is LIVE-ACCURATE speaker labels; the lead is FluidAudio (Apple-Silicon ANE), now
SPIKE-VALIDATED (2026-06-30 eve — see the latest progress-log entry + "SPIKE DONE" below). The remaining work is a
product decision (how far to pivot), not more derivation.** A session of experiments (2026-06-30) settled what does
NOT work, so a fresh context can skip re-deriving it:
- **ASR is not the problem.** `large-v3` (whisper.cpp/Metal) is accurate; the on-device pain was *diarization* + GPU
  contention, not transcription. Don't chase a new ASR for accuracy alone.
- **Torch-free live diarization is dead.** A wespeaker-embeddings + agglomerative-clustering prototype was degenerate
  on real audio — 66% window-agreement vs pyannote (= chance; both clusters were the same speaker). Scratch:
  `outputs/live_diarize_proto.py`.
- **Live rolling-window pyannote-on-MPS FAILED on-device** — branch `feat/live-diarization` (**unmerged**; built +
  ci-green but unusable). Three independent failures: (1) **GPU contention** — pyannote (MPS) and live `large-v3`
  (Metal) fight for the GPU, so the transcript falls progressively behind; (2) **over-segments anyway** (showed 5
  speakers, not 2 — the overlap-based anchoring is too weak); (3) **no graceful final pass** — stop cancels the loop,
  pyannote never catches up. Pyannote latency was measured (`outputs/pyannote_latency.py`): MPS is ~25x faster than CPU
  (60s window ~1s, RTF ~0.02), but contention + anchoring kill it regardless. **Recommend abandoning the branch.**

**THE LEAD — FluidAudio** (vetted 2026-06-30; `github.com/FluidInference/FluidAudio`): a Swift/CoreML package
(Apache-2.0) that runs ASR (Parakeet, 2.5% WER LibriSpeech) **and** diarization on the **Apple Neural Engine, bypassing
the GPU/MPS entirely** — the root fix for the contention. Ships streaming diarizers (LS-EEND ≤10 spk, Sortformer ≤4)
**and** the same pyannote Community-1 we validated, as CoreML (10.6% DER offline on AMI; ~26% streaming). Being Swift,
it fits the existing helper (helper does capture → could also do ASR + diarization on the ANE → stream results over the
existing IPC). Caveats: a real architecture pivot (audio-AI moves Python→Swift helper); pre-1.0 (v0.15.4); streaming
DER ~26% (better than ours, not "solved"); Parakeet's NVIDIA Open Model License needs a distribution check (fine
internally now).

**SPIKE DONE (2026-06-30 eve) — FluidAudio validated; see the progress log for the full numbers.** The standalone Swift
spike lives at `outputs/fluidaudio-spike/` (gitignored). Headline: on the two known-2-speaker clips, FluidAudio
**offline** (pyannote community-1 CoreML) nails **2**, **online clustering** gives 2-dominant (+<1% phantom), both on
the **ANE at 200-400x RTFx**, torch-free + ungated; Parakeet ASR (ANE) is large-v3-class. Caveat: streaming
over-segments on harder audio (offline is the reliable reference) — improved, not solved. To re-run:
`cd outputs/fluidaudio-spike && swift build -c release && .build/release/spike <offline|online|lseend|asr> <them.wav>`.

**CHOSEN (2026-06-30 eve, with the user): the PHASED pivot (A→B).** Move on-device AI into the Swift helper on the
ANE, smallest-risk-first. Licensing verified all-clean (FluidAudio Apache-2.0; diarization + Parakeet models both
CC-BY-4.0 + **ungated** — removes the HF-gate distribution blocker). The all-Swift-backend question (collapse the Python
API/DB into Swift too) is **parked as a Phase-5/distribution decision** — revisit once the AI is in Swift and we see how
thin Python gets; the deciding variable is whether distribution-to-non-tech-users becomes a near-term priority (going
all-Swift would delete the "bundle + notarize CPython" step, the roadmap's hardest). The IPC seam keeps it reversible.

**Increments (the plan):**
- **F1 — Offline diarizer in the helper (replaces the Python pyannote refine). DONE (2026-06-30 eve).** New Swift
  executable `hearsay-diarize` (in `helper/`, depends on FluidAudio 0.15.4, pinned in `helper/Package.resolved`; kept a
  separate target so the heavy CoreML dep never touches the lean capture binary) reads a wav → runs
  `OfflineDiarizerManager` (pyannote community-1 CoreML, ANE) → emits JSON speaker turns to stdout (all FluidAudio
  diagnostics go to stderr, so stdout is clean JSON). New Python `FluidAudioDiarizer` (an `OfflineDiarizer` impl,
  `diarization/offline.py`) writes the samples to a temp wav (FluidAudio's loader is file-based), subprocess-invokes
  the tool, parses turns; slots behind the existing seam so `rediarize_meeting` + both call sites (CLI `hearsay
  rediarize` + `POST /meetings/{id}/rediarize`) are **unchanged**. New `OfflineDiarizerKind` enum +
  `diarization.offline_backend` setting **defaults to FluidAudio**; pyannote stays opt-in (its `pyannote_model`/
  `hf_token`/`refine_device` fields apply only to it). **Validated:** the tool + the full Python→Swift→Python path both
  return **2 speakers / 102 turns** on the 1535 call (matches the spike + the Python pyannote we trust). +5 tests
  (mocked subprocess: JSON parse, helper-failure, missing-binary; default-backend; pyannote opt-in) → **144 pass; ruff
  + mypy --strict (69 files) + full `swift build` + `hearsay-helper selftest` all green**. **Win delivered: the default
  refine no longer needs torch, pyannote, or the HF gate.** Note: `swift build` now emits one benign warning from
  FluidAudio's *own* tree (a stray `benchmark.md` unhandled file) — third-party, build still exits clean. Uncommitted
  on `main` (no branch/commit yet — awaiting the user). (Batch/one-shot → subcommand + JSON stdout, not the live IPC;
  live streaming is F2.)
- **F1b — Auto-refine at finalize. DONE (2026-06-30 eve).** The refine now runs automatically when a meeting stops
  (inline in `SessionManager.stop_meeting` → `_maybe_auto_refine`), not just via the manual button — cheap now that F1
  put the diarizer on the ANE. New `diarization.auto_refine` setting (default on); gated on `refine` + `them.wav`;
  best-effort (never breaks the stop). Both stop paths (API + CLI `hearsay live`) covered. +4 tests; 148 pass; ruff +
  mypy green. Real trigger validates on the next live meeting.
- **F2 — Live diarizer over IPC.** Add FluidAudio online/streaming diarization to the capture helper; stream speaker
  labels over `control.sock` live; Python fuses them onto live segments. Replaces the torch-free online clusterer.
- **F3 — Parakeet ASR in the helper. DONE (2026-06-30 eve).** Persistent `hearsay-asr` sidecar (FluidAudio Parakeet
  TDT v3 on the ANE) + Python `ParakeetBackend` (owns the sidecar, round-trips utterances over stdio); `ASRBackend`
  gained `close()`; `ASRBackendKind.PARAKEET` is the **default** (whisper.cpp/mlx kept as fallback). Removes
  whisper.cpp/Metal from the live path (the unrecoverable-Metal failure source). ~100-230 ms warm/utterance; +7 tests;
  156 pass; swift build + selftest green. Kept Silero VAD (CPU, not the fragility source). Follow-up: the ASR-model
  picker UI is whisper-centric (no-op under Parakeet) — tidy in F4. On-device live validation pending.
- **F4 — Teardown.** Once F1-F3 validate, delete the now-dead Python asr/vad/diarization inference + their deps
  (torch, pyannote, onnxruntime, pywhispercpp, mlx, kaldi-native-fbank); shrink pyproject; refresh docs.

**(superseded) the three options that were on the table:** smallest→largest —
- **(A) Offline-refine swap only.** Replace the Python pyannote refine with FluidAudio's offline CoreML diarizer in the
  helper. Same model + accuracy we validated, but **removes both distribution blockers** (no ~2 GB torch, no HF-gated
  model) and runs on the ANE. Medium effort, high certainty, no live-path risk. Keeps live labels as-is (online
  clusterer) or upgrades them to FluidAudio online (still imperfect).
- **(B) Full audio-AI pivot.** Move **ASR (Parakeet) + live + offline diarization** into the Swift helper on the ANE,
  stream results over the existing IPC; retire whisper.cpp/Silero/pyannote from the live path. Biggest change
  (audio-AI moves Python→Swift), **structurally kills GPU contention**, best live labels available. Pre-1.0 dep risk;
  Parakeet's model license VERIFIED CC-BY-4.0 + ungated (2026-06-30 eve — upstream nvidia/parakeet-tdt-0.6b-v3 is CC-BY-4.0, not the restrictive NVIDIA Open Model License; commercial + redistribution OK with attribution).
- **(C) ~~Measure more before committing.~~ DONE (2026-06-30 eve):** ran FluidAudio online diarization + Parakeet ASR
  concurrently on the 567s clip — ~3-7% interference, both held ~400x real-time. The (b) contention risk is retired;
  the remaining fork is purely **A vs B** (the only un-measured thing left is a true end-to-end live capture, which is
  the validation step *after* whichever path is built).

**FALLBACK if not pivoting to FluidAudio:** keep `diarization.live` OFF (the failed live setting only exists on the
unmerged branch; `main` is already clean — online clusterer + the pyannote refine-on-stop default) and build
**auto-refine-at-finalize** (run the validated full-track refine automatically at stop → correct 2 speakers, name
post-stop). Small; reuses the built refine. The deferred **`turbo`-live + `large-v3`-finalize** ASR split (to free the
GPU) only matters on the GPU-whisper path.

**State of `main`:** everything through Phase 2 + the ASR-`large-v3` + pyannote-default work is merged and green; the
live-diarizer experiment is NOT on main (only `feat/live-diarization`). HF prereq cleared (user `crwiseman`; gated model
accepted; `hf auth login` done). Optional Phase 1 belt-and-suspenders still open (a both-speakers Me/Them interleave
run). Phase 3 (calendar roster + OCR active-speaker fusion) remains the nominal next *phase* but is deprioritized behind
getting live speaker labels right.

**Minor teardown note (2026-06-29):** a real `serve` log showed whisper.cpp Metal errors (`command buffer 0 failed
with status 3` → `failed to encode`/`decode`) **immediately before `ggml_metal_free: deallocating` + a meeting DELETE**
— i.e. at stop/app-kill, not a live mid-meeting failure (transcription did not stay dead; the user confirmed). Cause:
`pipeline.close()` cancels the per-stream consume tasks, but a detached `asyncio.to_thread(self._asr.transcribe, …)`
keeps running on the threadpool, so the Metal context can be freed (session GC on stop, or process exit on kill) while
an inference is still in flight. Cost is only the last in-flight utterance + scary log lines at stop. **Low priority.**
If ever fixed: join/await the in-flight ASR before teardown (note the cancel releases `_asr_lock` early, so the final
`segmenter.flush()` can briefly overlap a second inference on the same context).

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
- **ASR default (UPDATED 2026-06-30 — was `large-v3-turbo`):** whisper.cpp **`large-v3`** via `pywhispercpp`
  (Metal+CoreML), behind an `ASRBackend` protocol. An on-device A/B (Bill Murray Hot Ones, known content, via
  `scripts/transcribe_eval.py`) showed the **model**, not decoding params, drives accuracy: turbo/greedy heard
  "tongue" as "tone" at 1:02, beam+context didn't fix it, **large-v3 did**. Turbo stays the speed option; beam-search
  + context-carryover are implemented behind `ASRSettings` but **off by default** (kept for a future WER A/B).
- **ASR backends (2026-06-26):** don't phase-split — ship **both** behind `ASRBackend`, gated by install extra:
  whisper.cpp default (`asr` extra, torch-free) + **mlx opt-in** (`accel` extra; mlx-whisper pulls torch). Models are
  **swappable at runtime** via config / `PUT /api/asr/model` (next-meeting effect). pywhispercpp is the only torch-free ASR.
- **VAD (2026-06-26):** **Silero via `onnxruntime`** (torch-free; the `silero-vad` pip pkg hard-depends on torch).
  Behind a `VAD` protocol + a pure `Segmenter` (hysteresis, partial/final). Bug found+fixed: Silero needs 64 samples of
  left-context per 512-frame. Model is pinned + sha256-checked, fetched by `hearsay fetch-models`.
- **Dependency policy (2026-06-26):** vet every new dep **live** (latest version, no CVEs, permissive license MIT/BSD/
  Apache) before adding. Verified clean: pywhispercpp 1.5.0, onnxruntime 1.27.0, numpy 2.4.6, silero/mlx/pyannote.
- **Diarization (CHANGED 2026-06-26 — was `pyannote.audio` 4.0 community-1):** **torch-free + ungated by
  default** — a speaker-embedding ONNX model on the onnxruntime we already use (e.g. wespeaker / 3d-speaker;
  vet live in inc 2) + our own **online clustering** over the **Them** VAD utterances, behind a `Diarizer`
  seam. **pyannote 4.0 community-1 is now opt-in** (accuracy-max; pulls torch + needs an HF token + license
  accept). Reason: hearsay must ship as a **distributable package for non-technical users on low-spec machines**
  — pyannote's HF-token gating (can't ask non-tech users for an HF account) and ~2 GB torch bundle fail that;
  CC-BY-4.0 / Apache ONNX weights are redistributable + bundle-able, lighter, and faster. Bonus: per-utterance
  embeddings double as the **cross-meeting voiceprint** (recognize recurring people). (Supersedes the plan's
  pyannote-primary design; the plan file is unchanged.)
- **Diarization (RE-CHANGED 2026-06-30 — pyannote is now the DEFAULT):** with the user, "torch-free" is rescoped as a
  **Phase-5/distribution** concern (deferred), not a present rule — for internal run-from-source use, a couple GB of
  torch is an acceptable trade for accuracy + ecosystem support, and 2026-06-30 research confirmed **no torch-free
  diarizer is competitive** (pyannote/NeMo/DiariZen all lead and are torch). On-device (Inc 7e, 2026-06-30): the
  torch-free online clusterer reported **7 speakers** on a real 2-person call; the pyannote refine corrected it to
  **2**. So **pyannote `speaker-diarization-community-1` is the default diarization** (post-meeting refine;
  `diarization.refine` now defaults **on**, recording `them.wav`); the torch-free online clusterer stays the **live**
  labeler + the bundle-able fallback. **Privacy tradeoff:** raw `them.wav` is retained by default now (was off);
  configurable off, and delete-meeting removes the folder. Trigger stays explicit (the "Refine speakers" button /
  `hearsay rediarize`); **auto-refine-at-finalize is a noted follow-up** (heavy torch shouldn't silently block every
  stop; needs background exec + the served-vs-CLI lifecycle handled).
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

- **2026-06-30 (eve, F2 — turn-driven live Them in Swift, code-complete; needs live validation).** With the user,
  committed to the **full fully-Swift end state** (all audio-AI in Swift/ANE, Python = ML-free backend) via
  **architecture B** (lean capture binary stays a PCM streamer; AI in sidecars the core feeds; Python relays bytes but
  runs no models). Built F2 = turn-driven live Them, **no Python fusion** (the diarizer's turns ARE the segments, like
  the refine): new Swift **`hearsay-live`** sidecar (FluidAudio LS-EEND streaming diarizer + Parakeet) — Them PCM in →
  as each turn finalizes, transcribe it → emit `{speaker,text,start_s,end_s}`. **Validated standalone** on a recorded
  clip (20 turns, 2 speakers, accurate text, caught the back-and-forth). Python **`LiveThemProcessor`** owns the
  sidecar, feeds Them audio (offset-corrected), persists + broadcasts the turns (speaker→Speaker N + cluster); wired
  into the pipeline (Them skips VAD/ASR/clusterer) + session, behind `diarization.live_streaming` (default **on**). Me
  stays VAD+Parakeet. Online clusterer kept as the **off-fallback** (delete after on-device validation). Also fixed a
  latent CI bug: a bare `swift build` pulled in FluidAudio's broken CLI target — Makefile now builds explicit products.
  **154 pass; ruff + mypy --strict (70 files) + swift build + selftest green.** Commits `c5a39c4` (sidecar) `9b35e2d`
  (build fix) `60c138e` (wiring) on `feat/fluidaudio-pivot`. **NEXT (needs the user): restart serve + run a real
  meeting to validate live turn labels** (expect a few seconds of latency, correct speakers; refine still perfects at
  finalize). **Then the remaining fully-Swift steps:** (2) voiceprints from FluidAudio's embeddings → drop the ONNX
  embedder + kaldi; (3) Me → a Swift streaming-ASR sidecar → drop Silero; (4) drop whisper.cpp (Parakeet-only). After
  (4) Python has zero audio/ML deps. See [[hearsay-swift-pivot-direction]].
- **2026-06-30 (eve, F4 part 1 — torch removed).** With the validated FluidAudio path the default everywhere, deleted
  the torch-heavy fallbacks: the in-process **pyannote** offline diarizer (`PyannoteDiarizer` + the `offline_backend`/
  `pyannote_model`/`hf_token`/`refine_device` settings + `OfflineDiarizerKind` + the `diarization-pyannote` extra) and
  the **mlx** ASR backend (`MlxBackend` + the `accel` extra + the mlx model-resolution). `build_offline_diarizer` is now
  always FluidAudio; `build_asr` is Parakeet (default) or whisper.cpp. Dropped the `pyannote`/`torch`/`mlx_whisper` mypy
  overrides; `hearsay rediarize` lost its pyannote `--device` flag. **`uv lock` + `uv sync` pruned the whole torch tree
  (~2 GB: torch, torchaudio, torchcodec, triton, …) — `torch`/`pyannote` confirmed gone from the venv.** This kills the
  biggest distribution blocker (a 2 GB bundle). **153 pass; ruff + mypy --strict (69 files) + licenses + pip-audit
  green** (the torchcodec UNKNOWN-license noise is gone too). Kept on purpose: **whisper.cpp** as a torch-free ASR
  fallback (`HEARSAY_ASR__BACKEND=whispercpp`) + its model picker; **Silero VAD** (live chunking) + the **ONNX voiceprint
  embedder** (cross-meeting recall) — both onnxruntime, no torch, still used. Remaining F4: optionally drop whisper.cpp +
  the picker for a pure-FluidAudio build (user's call); a docs/CLAUDE.md sweep (still say pyannote/torch). Committed on
  `feat/fluidaudio-pivot`. **Next: F2 (live diarization in Swift).**
- **2026-06-30 (eve, turn-accurate refine — splits overlapping/quick-turn-taking talkers).** On-device, Parakeet
  accuracy + quick finalization were great, but slightly-overlapping talkers weren't split. **Diagnosed:** the Silero
  VAD (no max-duration cap, still Python) merged a full back-and-forth into one 56 s utterance, so the diarizer's fine
  turns (S1/S2/S1) collapsed to the dominant speaker at the segment level — the Phase-2 "segmentation ceiling." Any VAD
  splits on silence, not speaker change, so the fix isn't a different VAD: **segment by the diarizer's turns.** Rebuilt
  the refine — `rediarize_meeting` now, for each FluidAudio turn, slices that turn's audio and **re-transcribes it with
  Parakeet** (the sidecar from F3), emitting one correctly-labeled segment per turn. New `SpeakerService.apply_turn_
  diarization` (replaces `apply_diarization`): drops the coarse Them segments + clusters, creates Speaker 1..N, inserts
  one segment per turn (Me untouched). Manual-rename carry-forward + voiceprint recognition preserved (factored
  `_carry_forward_names`); a no-turns result leaves the transcript intact (no wipe). **The finalized transcript is now
  fully FluidAudio/ANE-driven — Silero no longer shapes it** (it remains only the live chunker feeding Parakeet during
  the meeting). **Validated on the real 2019 hot-ones-vaughn meeting**: the 56 s "Speaker 1" block became S1[1.7-28.7]/
  S2[28.7-40.6]/S1[40.6-57.1], each with its own text. +3 tests rewritten (turn-rebuild) → **156 pass; ruff + mypy
  --strict (70 files) green.** Cost: auto-refine now re-transcribes every turn (a 2nd short-lived Parakeet sidecar at
  finalize) — still a few seconds. Committed on `feat/fluidaudio-pivot`. (Honest state: VAD + live-diar still Python;
  F2 would move live diar to Swift. The *finalized* path is now Swift end-to-end.)
- **2026-06-30 (eve, F3 built — Parakeet ASR on the ANE replaces whisper.cpp/Metal in the live path).** Directly fixes
  the whisper Metal failure above. New persistent Swift sidecar **`hearsay-asr`** (FluidAudio Parakeet TDT v3): loads the
  model once, then serves a stdio request/response loop — request `<uint32 LE n><n float32 LE>` (one VAD utterance) →
  response `{"text":...}\n`; EOF exits. New Python **`ParakeetBackend`** (`ASRBackend` impl) owns the sidecar for the
  meeting, round-trips each utterance over pipes (threading-lock-guarded against the stop-teardown race), parses text.
  Added `close()` to the `ASRBackend` protocol (whisper/mlx no-op; Parakeet terminates the sidecar) + `pipeline.close()`
  calls it after the final flush. New `ASRBackendKind.PARAKEET` + `asr.backend` **defaults to it** (whisper.cpp/mlx stay
  available; Silero VAD unchanged — it's CPU, not the fragility source). **Measured:** warm transcribe of a real
  utterance **~100-230 ms** (model load is a one-time ~11 s at sidecar start); transcript quality matches whisper
  large-v3 (validated on the captured Vince Vaughn clip via the real sidecar + via `build_asr(Settings())`). +7 tests
  (mocked sidecar: framing/parse/empty/EOF/missing-binary/close + default-backend + path) → **156 pass; ruff + mypy
  --strict (70 files) + full swift build (3 executables) + selftest green.** Follow-up: the `GET/PUT /api/asr/model`
  picker is still whisper-model-centric (ignored under Parakeet) — cleanup in F4. **The user must restart `serve` to pick
  up the Parakeet default.** Uncommitted-then-committed on `feat/fluidaudio-pivot`. **Next: on-device validation of a
  live meeting on Parakeet, then F2 (live diarization) or F4 (teardown).**
- **2026-06-30 (eve, on-device auto-refine test → whisper-Metal failure diagnosed + a guard added).** User ran a real
  meeting (`hot_ones_vaughn`) via `serve`, hit stop, saw "1 speaker / no Them turn". **Diagnosis (not an auto-refine
  bug):** capture worked (them.wav = 54.7 s real audio, RMS 0.031) but the meeting had **0 transcribed segments** — in
  the long-running serve process **whisper.cpp's Metal backend hit a transient `command buffer failed (status 3)` and
  entered an unrecoverable error state** ("recreate the backend to recover"); whisper.cpp doesn't auto-recreate, so the
  whole meeting produced no finals. Confirmed whisper itself is fine: transcribing the *same* them.wav offline returned
  the correct Vince Vaughn intro. Auto-refine then ran on the empty meeting and minted a phantom "Speaker 1" (the "1
  speaker" seen). **Fix:** `rediarize_meeting` now **skips when there are 0 Them segments** (no diarize, no phantom
  cluster) — reordered to read segments first + early-return; +1 test (`test_rediarize_skips_when_no_them_segments`);
  **149 pass; ruff + mypy green.** Short-term workaround: restart `serve` to recreate the whisper backend. **This is
  exactly the whisper.cpp/Metal fragility the pivot's F3 (Parakeet ASR on the ANE) eliminates — strong evidence to
  prioritize F3.** (The user's `hot_ones_vaughn` meeting row is empty + has a phantom cluster; safe to delete in the UI.)
  Uncommitted-then-committed on `feat/fluidaudio-pivot`.
- **2026-06-30 (eve, latest)** **Auto-refine at finalize built (the quick win F1 unlocked).** With the user, chose to
  do this before the heavier live-streaming F2: now that the default offline diarizer is FluidAudio on the ANE
  (~seconds, not 30s+ of torch), the refine runs **inline when a meeting stops** instead of only via the manual "Refine
  speakers" button. Single hook in `SessionManager.stop_meeting` → `_maybe_auto_refine(meeting)`: gated on
  `diarization.refine && diarization.auto_refine` (new setting, default **on**) + `them.wav` existing; best-effort
  (a missing recording or a diarizer error is logged, **never** breaks the stop). Both stop paths (API
  `POST /meetings/{id}/stop` + CLI `hearsay live`) funnel through `stop_meeting`, so both get it. Inline-await (not a
  background task) sidesteps the served-vs-CLI lifecycle problem the TODO flagged — viable only because F1 made the
  refine fast. +4 tests (runs / disabled / no-recording / error-tolerant) → **148 pass; ruff + mypy --strict (69 files)
  green** (also fixed an `__all__` sort RUF022 that F1's targeted lint missed). Real stop→refine validates on the next
  live meeting (now automatic). Uncommitted on `feat/fluidaudio-pivot`. **Next: F2 (live diarizer over IPC), still
  open.**
- **2026-06-30 (eve, later)** **Phased pivot chosen + increment F1 built — FluidAudio offline diarizer in the Swift
  helper, replacing the default Python pyannote refine.** With the user (after the licensing questions resolved
  all-clean — FluidAudio Apache-2.0; diarization + Parakeet models both CC-BY-4.0 + ungated), chose the **phased**
  pivot (A→B), parking the all-Swift-backend question as a Phase-5/distribution decision. Built **F1**: new Swift
  `hearsay-diarize` tool (FluidAudio `OfflineDiarizerManager`, ANE → JSON turns on stdout) + Python `FluidAudioDiarizer`
  (`OfflineDiarizer` impl, subprocess + temp wav + JSON parse) + `OfflineDiarizerKind`/`offline_backend` setting
  **defaulting to FluidAudio**; pyannote opt-in. Drop-in behind the existing seam — `rediarize_meeting` + both call
  sites unchanged. Validated 2 speakers / 102 turns end-to-end on the 1535 call. **144 pass; ruff + mypy --strict + full
  swift build + selftest green.** Removes torch + pyannote + the HF gate from the default refine. Full detail in the F1
  increment entry below. Uncommitted on `main`. **Next: F2 (live diarizer streaming over IPC).**
- **2026-06-30 (eve)** **FluidAudio spike DONE — measured on the recorded `them.wav`; the lead is validated, the
  full pivot is the open decision.** Built a throwaway Swift spike (`outputs/fluidaudio-spike/`, gitignored) on
  **FluidAudio 0.15.4** (Apache-2.0, vetted live; actively maintained, 61 releases). The bundled `fluidaudiocli` won't
  link — a Swift type-checker timeout in an unrelated Nemotron *benchmark* file — so the spike links the `FluidAudio`
  **library** directly (read the real API from the resolved checkout's CLI commands; don't trust the docs). Ran 3
  diarizers + Parakeet ASR against the recorded clips; all CoreML models auto-download from public `FluidInference/*`
  HF repos (**no gate, no torch**). **On the 2 known-2-speaker clips** (Hot Ones interview; the 1535 real call where
  our torch-free clusterer reported **7**): FluidAudio **offline** (pyannote community-1 CoreML) = exactly **2** on
  both (RTFx 28x cold / 280x warm); **online clustering** (pyannote-3.1 CoreML) = 2 dominant + a <1% phantom → **2**
  after a trivial min-talk filter; **LS-EEND** = 2 (count right, talk balance poor — 79/6 on Hot Ones vs true ~34/46).
  All on the **ANE at 200-400x RTFx**. **Parakeet TDT-v3 ASR (ANE)** transcribed Hot Ones at whisper-large-v3-class
  quality, 3.6x RTFx (cold, incl. model compile). **Caveat (no over-claim):** on the 3 clips *without* ground truth,
  offline (1-3 spk) and online (2-5 spk) **disagree** — online over-segments on harder audio, and offline *merged* the
  two speakers of the manually-`vader`-labeled 1619 clip. So streaming is **improved, not solved** (matches the docs'
  ~26% streaming DER); **offline** (the same model we already trust in Python) is the reliable reference. **Verdict:**
  (a) streaming holds ~2 on clean audio — **YES**; (b) ANE frees the GPU — **YES, measured** (not just structural):
  running FluidAudio online diarization **and** Parakeet ASR concurrently on the 567s clip cost ~3-7% — diarization
  398x→370x, ASR 410x→399x, both still ~400x real-time. The exact opposite of the pyannote-MPS-vs-whisper-Metal
  contention that fell progressively behind. CoreML co-schedules two AI workloads with negligible interference. (Warm
  Parakeet ASR is 410x — the earlier 81s/292s was almost all first-run CoreML compile.) Two clean wins fell out: an
  **offline-refine
  swap** (FluidAudio offline = our validated pyannote, but torch-free + ungated + ANE → kills both distribution
  blockers) and **ANE ASR** (kills GPU contention). The **full Python→Swift audio-AI pivot is the open decision** — see
  Pick up here. Nothing merged to `main`; spike is gitignored scratch.
- **2026-06-30 (pm)** **Live-accurate diarization investigated end-to-end; rolling-window pyannote FAILED on-device;
  FluidAudio (Apple ANE) chosen as the lead. No code merged to `main`.** Motivation: the online clusterer shows 7-8
  phantom speakers live, making naming-before-refine + dedup painful. Ruled out torch-free live (a wespeaker
  embeddings + AHC prototype scored 66% / chance vs pyannote, degenerate; `outputs/live_diarize_proto.py`). Measured
  pyannote latency (`outputs/pyannote_latency.py`): MPS ~25x faster than CPU (60s window ~1s). Built a rolling-window
  pyannote-on-MPS live diarizer on branch **`feat/live-diarization`** (`LiveDiarizer` + overlap anchoring + 3
  `SpeakerService` methods + `diarization.live*` settings; +4 tests; full `make ci` green) — but it **failed
  on-device**: GPU contention (pyannote-MPS vs live large-v3-Metal → transcript falls behind), over-segments anyway
  (5 speakers not 2; weak anchoring), no graceful final pass on stop. **Branch unmerged; recommend abandoning.**
  Confirmed whisper `large-v3` ASR is fine (not the bottleneck). **Research win:** vetted **FluidAudio** (Swift/CoreML,
  Apache-2.0) — ASR (Parakeet) + diarization on the **ANE, bypassing the GPU** (the contention root-fix); ships
  streaming diarizers + pyannote-CoreML (10.6% DER AMI). The lead; next is a standalone Swift spike on the recorded
  them.wav. Scratch prototypes live under `outputs/` (gitignored). See the refreshed "Pick up here" up top.
- **2026-06-30** **ASR default → large-v3 (measured) + pyannote promoted to the default diarization; Inc 7e validated.**
  **ASR:** built `scripts/transcribe_eval.py` (offline VAD+ASR eval, mirrors the live pipeline) and ran a real A/B on a
  recorded clip (Bill Murray Hot Ones, known content). At 1:02: baseline turbo/greedy → "tone"; turbo+beam+context →
  still "tone"; **large-v3 → "tongue"**. So the model — not decoding params — drives accuracy → defaulted to
  `large-v3` (turbo stays the speed option). Beam-search + per-stream context-carryover are implemented behind
  `ASRSettings` (`beam_size`/`condition_on_previous_text`, `prompt` threaded through `ASRBackend`) but **off by
  default** (neutral on the test; beam slows live). The eval caught a real bug — whisper.cpp's `beam_search` struct
  needs both `beam_size` AND `patience` (`KeyError` otherwise; would have broken live the moment beam ran). +1 test.
  **Diarization:** with the user, torch-free is rescoped to a Phase-5/distribution concern (deferred), so **pyannote is
  now the default** — `diarization.refine` defaults **on** (records `them.wav`); the online clusterer stays the live
  labeler + fallback. **Inc 7e passed on-device**: the online path reported 7 speakers on a real 2-person call, the
  pyannote refine corrected it to 2. Privacy tradeoff logged (raw audio retained by default; configurable off; trigger
  stays the explicit "Refine speakers" button / `hearsay rediarize`, auto-at-finalize deferred). Full `make ci` green
  (139 tests). Merged `feat/asr-accuracy` to `main` (`f84a3f1`); this diarization flip on `feat/pyannote-default`.
- **2026-06-29** **Cross-meeting voiceprint recognition — a returning person is auto-named from their voiceprint.**
  Closes the diarization vision ("embeddings double as the cross-meeting voiceprint"). No migration — reuses the
  existing `clusters.centroid` BLOB. New pure-stdlib `diarization/voiceprint.py` (float32 (de)serialize +
  cosine `match_identity`, fully CI-tested). `rediarize_meeting` gains an injected `embedder`: it embeds each pyannote
  speaker's concatenated audio (`OnnxSpeakerEmbedder`, off-loop, capped 12 s), stores the centroid on the cluster, and
  matches against `SpeakerService.known_voiceprints` (locked+named clusters from *other* meetings) at
  `recognition_threshold` (0.6, conservative). `apply_diarization` stores centroids + auto-binds a recognized speaker
  **provisionally (not locked)** so a manual rename still wins, and only **locked** (manually confirmed) voiceprints
  seed future recognition (no auto-recognition drift). CLI + API build the embedder and pass it (graceful `None` when
  the model is absent → recognition off, current behavior). +5 tests (4 pure + an end-to-end "name Alice in M1 →
  auto-recognized in M2, provisional/unlocked"). **138 pass; full `make ci` green.** Uncommitted on
  `feat/phase-2-diarization`.
- **2026-06-29** **Re-diarize now preserves manual renames (guardrail fix).** On-device, `rediarize` relabeled a real
  multi-person clip "more or less correctly"; the user asked how a manual rename survives a re-run. It didn't —
  `apply_diarization` deleted all clusters and recreated plain "Speaker N", wiping locked identities (violates the
  "manual labels lock" guardrail). Fixed: `rediarize_meeting` reads the locked identities before re-clustering and
  votes each manual name onto the new pyannote ordinal its segments most overlap (greedy one-name↔one-ordinal);
  `apply_diarization` gained `ordinal_names` and re-binds + locks + labels those clusters with the name instead of
  "Speaker N" (factored a shared `_get_or_create_identity`). So rename↔rediarize order no longer matters within a
  meeting; pyannote's unstable SPEAKER_xx labels don't lose names. +1 test (`test_rediarize_preserves_manual_rename`)
  → **131 pass; `make ci` green.** Cross-meeting auto-recognition (seed the diarizer with a stored voiceprint) still
  unwired — names cross meetings only as rename-box suggestions.
- **2026-06-29** **First on-device refine run — pipeline works end-to-end; test clip was single-speaker (inconclusive)
  + killed the torchcodec warning spam.** `hearsay rediarize latest` loaded community-1, diarized, relabeled, and
  rewrote the transcript with **zero** errors. Result was "9 turns, 1 speaker" on a 44 s clip whose transcript reads
  as one person's running commentary (no dialogue) — almost certainly correct, not a real multi-person test (pyannote
  multi-speaker detection is already proven here: JFK|english → 2). them.wav was clean (44 s, no clipping, RMS 0.023)
  and the offset sidecar (0.019 s) round-tripped. Fixed the brutal UX: pyannote's torchcodec/ffmpeg load emits a
  multi-line traceback as a `UserWarning` at import (message starts with `\n`, so a `.*torchcodec.*` filter misses) —
  now wrapped the lazy `from pyannote.audio import Pipeline` in `warnings.catch_warnings()` + `simplefilter("ignore")`,
  and quieted the httpx model-download INFO logs. Verified: 0 spam lines. **130 pass; `make ci` green.** Next: a real
  2+ distinct-speaker recording to validate separation (and the granularity caveat — coarse multi-speaker VAD blocks).
- **2026-06-29** **Pyannote post-meeting refine built end-to-end (Inc 7b + 7c) — green; on-device validation is the
  last step.** Cleared HF access (user is `crwiseman`; logged in via `hf auth login`, accepted the gated
  `speaker-diarization-community-1` terms). Verified the pyannote 4.0.5 API live (`from_pretrained(..., token=)` →
  `DiarizeOutput.exclusive_speaker_diarization.itertracks`). **7b:** `diarization/offline.py` — `OfflineDiarizer`
  protocol + `SpeakerTurn` + `PyannoteDiarizer` (lazy torch; in-memory waveform tensor so torchcodec/ffmpeg never
  decode — ffmpeg is absent here, so a file-path would fail) + `build_offline_diarizer`; settings `pyannote_model` /
  `hf_token` (SecretStr → defaults to the CLI login) / `refine_device`. Installed `diarization-pyannote` (torch
  2.12.1 + pyannote 4.0.5 on cp314); **`make licenses` + `pip-audit` both green** over the torch tree (torchcodec
  license UNKNOWN is a metadata gap, actually BSD-3). Real-audio check: JFK|english|JFK → 2 speakers with both JFK
  spans merged. **7c:** pure `order_speakers` + `assign_segment_speaker` (offset-shifted max-overlap) +
  `SpeakerService.apply_diarization` (atomic replace-clusters + relabel) + `transcript/refine.py::rediarize_meeting`
  (read them.wav + offset sidecar → off-thread diarize → map → relabel → rewrite transcript via the finalize path) +
  recorder offset sidecar + CLI `hearsay rediarize <id|latest>`. +10 tests (3 seam, 4 mapping, 1 e2e relabel, 2
  sidecar) → **130 pass; full `make ci` green** (mypy 70 files; torch added to mypy overrides). Uncommitted on
  `feat/phase-2-diarization`. Deferred: 7d (API endpoint + UI button). Next: 7e (on-device validation).
- **2026-06-29** **Second on-device verify still bad → chose pyannote post-meeting refine; Inc 7a (Them recorder) done.**
  Re-ran after the tuning pass: still jumbles multiple people into one block. Transcript evidence + reading
  `vad/base.py` pinned the real cause — `min_silence_ms=600` with no max-duration cap makes the VAD emit 20–30 s
  utterances that already contain several speakers, so the one-embedding-per-utterance design can't separate them (a
  segmentation ceiling, not a threshold issue). Vetted pyannote live: **pyannote-audio 4.0.6** (released 2026-06-29,
  torch-based, py≥3.10) + model **`pyannote/speaker-diarization-community-1`** (CC-BY-4.0, gated → needs a free HF
  token, mono-16k input, has `exclusive_speaker_diarization` to reconcile with transcript timestamps). User chose the
  **pyannote post-meeting refine** path (accuracy-max opt-in; torch-free default preserved for distribution). Built
  **Inc 7a**: `transcript/recorder.py::ThemAudioRecorder` (streaming stdlib-`wave` writer) records the Them track to
  `<folder>/them.wav` only when `diarization.refine` is on (new setting, **off by default** — raw audio otherwise not
  retained), capturing the first sample's meeting-time offset so a later pyannote pass maps turns back onto segment
  timestamps. Wired into `TranscriptionPipeline` (records Them only, never Me; closed at finalize) + `SessionManager`.
  +2 recorder tests + pipeline-records-Them assertion → **120 pass; full `make ci` green**. Uncommitted on
  `feat/phase-2-diarization`. Next: 7b (pyannote backend — needs the user's HF token + torch install), then 7c
  (turn→segment mapping + relabel + transcript rewrite, manual `rediarize` trigger).
- **2026-06-29** **First on-device diarization verify → weak Them separation → torch-free tuning pass + diagnostics.**
  User ran a real multi-person call: Me/Them channel split holds, but Them speaker separation is poor — distinct
  people merge and the same person is re-numbered / not recognized on return. Read the clustering + feature code:
  **no bug**, a design ceiling of greedy single-pass online clustering (running-mean centroid drifts irrevocably from
  short/overlapping clips; single global cosine threshold 0.5 is a clean-speech guess that real-call audio compresses).
  Shipped three no-regret, still-torch-free changes: (1) **duration-weighted centroid** — `OnlineSpeakerClusterer.assign`
  gains `weight=` (the utterance seconds) so a short noisy clip can't tilt a voiceprint as much as a long clean one
  (`MeetingDiarizer` passes `duration_ms/1000`); (2) **`min_embed_ms` 500→1000** — sub-second turns stay generic "Them"
  rather than mis-attributed; (3) **per-utterance INFO log** in `MeetingDiarizer.resolve`
  (`them diarized: dur=… speaker=… new=… cos=… total_speakers=…`) since we were tuning blind — one more call now yields
  the real cosine distribution. Confirmed both knobs are already settings-plumbed
  (`HEARSAY_DIARIZATION__CLUSTER_THRESHOLD` / `__MIN_EMBED_MS`). +1 fusion test (centroid is duration-weighted) →
  **118 pass; full `make ci` green** (ruff + mypy 67 files + swift selftest + audit + licenses). Uncommitted on
  `feat/phase-2-diarization`. Next: re-run the call, read `cos=`, tune the threshold; escalate to a stronger ungated
  ONNX embedder or pyannote opt-in only if tuning isn't enough.
- **2026-06-26** **Phase 2 increment 6 (finalize) done — resolved names bake into the final transcript; only the
  on-device verify remains (needs the user).** Confirmed (rather than rebuilt) the finalize behavior: `bind_cluster`
  retroactively relabels a speaker's segments (inc 5a) and the finalize path (`_ordered_lines` → atomic
  `sink.finalize`, temp + os.replace) re-reads the DB, so a mid-meeting rename bakes the name into the final
  `transcript.md`. New end-to-end test: rename "Speaker 1" → "Alice" mid-meeting, finalize, assert the transcript
  contains "Alice" and not "Speaker 1". **117 pass; full `make ci` green.** **Phase 2 is code-complete (inc 1–6);
  the multi-person on-device verify is the user's step.**
- **2026-06-26** **Phase 2 increment 5b (UI rename) done.** React `SpeakerPanel` in the transcript view: lists the
  meeting's diarized speakers (`useSpeakers`, polled) each with an inline rename input backed by an
  identity-suggestion `<datalist>` (`useIdentities`); `useRenameSpeaker` PUTs the name and invalidates the meetings +
  identities query trees so the transcript relabels and suggestions refresh. Typed query-key factory + `api/types`
  aliases extended (SpeakerRead/IdentityRead/PageSpeaker/PageIdentity); theme-matched CSS. web tsc + vite build green;
  codegen unchanged (5a regenerated the contract). On `feat/phase-2-diarization`, uncommitted. Next: inc 6 (finalize + verify).
- **2026-06-26** **Phase 2 increment 5a (speaker API) done.** Backend for naming speakers: `GET
  /api/meetings/{id}/speakers` (clusters with resolved label = identity or "Speaker N"; identity eager-loaded),
  `PUT .../speakers/{cluster_id}` (rename → `SpeakerService.bind_cluster`: get-or-create identity, lock,
  **retroactively relabel that cluster's segments** in one bulk write; `SessionManager.relabel_speaker` also
  **propagates to the live clusterer** so an active meeting's future utterances carry the name), `GET /api/identities`
  (paginated suggestions). `SegmentRead` gains `cluster_id`; new schemas `SpeakerRead`/`SpeakerRename`
  (strip + min_length)/`IdentityRead`. Thin relays `MeetingDiarizer.bind` → `pipeline.bind_speaker` →
  `MeetingSession.bind_speaker` reach the live clusterer. OpenAPI + web TS types regenerated. +4 tests (relabel
  service; speakers/identities empty; rename 404 + blank-name 422) → **116 pass; `mypy --strict` + ruff + web
  tsc/build green** (web-codegen-check passes once committed). On `feat/phase-2-diarization`. Next: inc 5b (UI rename).
- **2026-06-26** **Phase 2 increment 4 (pipeline integration) done — diarization live in the pipeline.** New
  `transcript/MeetingDiarizer` (one per meeting) wraps embedder + `OnlineSpeakerClusterer` + cluster-row persistence
  behind one async `resolve(utterance)`; the `TranscriptionPipeline` calls it for finalized **Them** utterances
  (`_resolve_speaker`) — embed off-loop (`to_thread`) → `assign` → label (`identity` or "Speaker N") → persist
  `segments.cluster_id` (added to `MeetingService.add_segment`) + broadcast the resolved label. Me stays
  channel-labeled; partials keep the cheap channel label (only finals are embedded/clustered); short utterances
  (< `min_embed_ms`) stay generic "Them". `SessionManager` builds the diarizer via an injected `embedder_factory`
  that **degrades gracefully** (no model → `None` → "Them"), so a missing `fetch-models` never breaks capture.
  (Kept the pipeline constructor a clean DI seam: bundled the diarization deps into `MeetingDiarizer` rather than
  growing the arg list; one `# noqa: PLR0913`.) +2 pipeline tests (two distinct Them speakers → Speaker 1/2 with
  cluster_ids + Me by channel; no-diarizer degradation) → 112 pass; full `make ci` green. On
  `feat/phase-2-diarization`, uncommitted. Next: inc 5 (API + UI rename).
- **2026-06-26** **Phase 2 increment 3 (fusion engine) done — pure online speaker clustering.** Built `fusion/`:
  `OnlineSpeakerClusterer` (**pure stdlib, no numpy**, so it's dependency-free + fully CI-tested). Each Them
  utterance embedding matches the nearest existing speaker by cosine to a running centroid; at/above `threshold`
  (default 0.5, configurable via `DiarizationSettings.cluster_threshold`) it joins + updates the centroid, else
  starts a new speaker. "Speaker N" ordinals by first appearance; `add_seed(identity, centroid)` recognizes a
  returning person from a prior meeting (provisional naming on first utterance; an active speaker beats a seed);
  `bind(ordinal, name)` manually labels + locks. +11 pure unit tests (distinct/same voices, threshold edges,
  ordinals, centroid mean, seed pre-bind, lock) + a guarded **end-to-end real-speech test** (embedder + clusterer
  group JFK's 3 chunks → 1 speaker, a different speaker stays separate → 2 speakers, counts [1, 3]). **110 pass;
  `mypy --strict` + ruff clean.** On `feat/phase-2-diarization`, uncommitted. Next: inc 4 (wire into the pipeline).
- **2026-06-26** **Phase 2 increment 2 (embedding seam) done — torch-free, validated on real speech.** Vetted the
  diarization stack live: **sherpa-onnx's macOS PyPI wheels are broken** (ship no onnxruntime dylib, cp313 +
  cp314 alike), so the torch-free path is **onnxruntime-direct**: a CC-BY-4.0 ONNX speaker embedder + **kaldi-native-fbank**
  (Apache-2.0 — the exact reference features, so no hand-rolled fbank risk) on the onnxruntime we already use.
  Built `diarization/`: `SpeakerEmbedder` protocol, `compute_fbank` (knf 80-dim Kaldi fbank + CMN),
  `OnnxSpeakerEmbedder` (lazy onnxruntime, fbank→`[1,T,80]`→`[1,512]`), `manager` (pinned + sha256 model registry +
  `download_embedding_model` + `build_embedder`). Default model **wespeaker CAM++_LM** (512-d, 29 MB, ungated,
  redistributable → bundle-able). `DiarizationSettings` + `DiarizationBackendKind` enum; `hearsay fetch-models`
  pulls it. pyproject: `diarization` = onnxruntime+numpy+kaldi-native-fbank; pyannote → opt-in `diarization-pyannote`.
  **Real validation:** the embedder separates speakers — JFK-halves cos **0.84** vs JFK-vs-other **0.22–0.33**.
  +5 tests (registry/path/checksum need no heavy deps; guarded embedder unit-norm/determinism + speaker
  discrimination) → **98 pass; `make ci` + licenses + audit green**. On `feat/phase-2-diarization`, uncommitted.
  Considered the torch route w/ the user: gating is bundle-able (CC-BY-4.0) and torch is only ~0.5 GB on Mac, so
  the real reason to stay torch-free is **low-spec RAM/compute** — user chose "keep light". Next: inc 3 (fusion).
- **2026-06-26** **Phase 2 increment 1 (DB foundation) done + diarization approach pivoted.** With the user:
  diarization moves **off pyannote** to a **torch-free, ungated** default (ONNX speaker embeddings on the
  onnxruntime we already use + our own online clustering over Them VAD utterances; `Diarizer` seam, pyannote
  kept opt-in) — driven by the product goal of a **distributable package for non-technical users on low-spec
  machines** (no HF-token gating, no ~2 GB torch, smaller/faster, redistributable weights; embeddings double as
  the cross-meeting voiceprint). DB foundation built (diarizer-agnostic, so not wasted by the pivot): `Identity`
  + `Cluster` models (`clusters` = `ordinal`→"Speaker N", nullable `identity_id` ON DELETE SET NULL, `locked`
  manual-lock, `centroid` voiceprint BLOB, unique `(meeting_id, ordinal)`) + `segments.cluster_id` (SET NULL);
  `SpeakerService` (create/list clusters, assign segment→cluster, bind cluster→identity get-or-create + lock,
  list identities). Migration `a69ee62a795b` — had to **name the batch FK** (SQLite batch ALTER requires it);
  the real DB was `create_all`'d + never alembic-stamped, so stamped baseline then upgraded (data intact: 1
  meeting / 34 segments; `alembic check` zero drift on real + fresh DBs). +7 tests → 93 pass; `mypy --strict` +
  ruff clean. On `feat/phase-2-diarization`, uncommitted. Next: inc 2 (vet the ONNX embedder live + build the seam).
- **2026-06-26** **Phase 1 closed — validated on a real meeting.** Ran the served pipeline on a real ~7-min
  meeting (listen-only → Them-only, which is correct): clean stop/finalize, real-time finals, and the timestamp
  fix confirmed — Them spans the full meeting on the shared `host_ts` clock (created_at tracks wall-clock) vs.
  the old ~100s drift. Me/Them interleaving with both speakers wasn't exercised (no Me speech) but the mechanism
  is proven. **Phase 1 (capture → VAD → ASR → DB + `transcript.md` + loopback API + React UI) is done; next is
  Phase 2 (diarization).**
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
- [x] Verify (Phase 1 exit): validated on a real ~7-min meeting — clean stop/finalize, real-time finals with
      correct wall-clock timestamps, channel separation holds (the cross-stream ordering bug is fixed; Them now
      tracks the shared `host_ts` clock). Me/Them interleave with both speakers wasn't exercised (listen-only
      run) but the mechanism is proven.

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

**Verify (phase exit):** a multi-person call shows stable Speaker 1..N for Them with Me separate; a manual
rename persists, locks the binding, and is suggested next meeting. Diarization is **torch-free ONNX embeddings
+ online clustering** by default (see the changed Decision above), pyannote opt-in.

- [x] **Inc 1 — DB foundation** (diarizer-agnostic): `identities` + `clusters` (`ordinal`/`identity_id`/`locked`/
      `centroid`) + `segments.cluster_id`; migration `a69ee62a795b`; `SpeakerService`; +7 tests. **Done.**
- [x] **Inc 2 — Diarization/embedding seam** (`diarization/`): `SpeakerEmbedder` protocol + torch-free
      `OnnxSpeakerEmbedder` (onnxruntime + kaldi-native-fbank reference features) + pinned + sha256 model
      registry/fetch; default **wespeaker CAM++_LM** (CC-BY-4.0, ungated). Validated on real speech:
      same-speaker cos 0.84 vs different 0.22–0.33. **Done.** (Deferred: a deterministic stub embedder lands
      with the inc-3 fusion tests; the pyannote opt-in backend is unbuilt — `diarization-pyannote` extra reserved.)
- [x] **Inc 3 — Fusion engine** (`fusion/`): pure-stdlib `OnlineSpeakerClusterer` — cosine + running centroid +
      threshold → stable first-appearance "Speaker N"; `bind` (manual lock) + `add_seed` (cross-meeting voiceprint
      pre-bind). 11 pure unit tests + an end-to-end real-speech clustering test. **Done.** (segment→cluster
      *persistence* + the channel split live in inc 4's pipeline wiring.)
- [x] **Inc 4 — Pipeline integration**: `MeetingDiarizer` wired into `TranscriptionPipeline` — each finalized Them
      utterance embedded → clustered → labeled ("Speaker N"/identity) → persisted with `segments.cluster_id`;
      `clusters` rows created on first appearance; Me channel-labeled; graceful degrade to "Them" with no model.
      +2 pipeline tests. **Done.** (Mid-meeting *retroactive* WS relabeling on manual rename is inc 5.)
- [x] **Inc 5 — API + UI**: `GET /speakers` + `PUT /speakers/{id}` rename (binds/locks + retroactively relabels
      segments + propagates to the live clusterer) + `GET /identities`; `SegmentRead.cluster_id`; React `SpeakerPanel`
      rename control with identity-suggestion datalist; OpenAPI→TS regenerated. **Done.** (5a backend / 5b UI.)
- [x] **Inc 6a — Finalize**: the atomic `transcript.md` rewrite re-reads the (relabeled) segments, so a mid-meeting
      rename bakes resolved names into the final transcript (end-to-end test). **Done.**
- [ ] **Inc 6b — On-device verify (NEEDS THE USER)**: real multi-person call → stable Speaker 1..N (Them) with Me
      separate; rename a speaker, confirm it persists + is suggested next meeting. (Default path: no HF token.)
      **Result (2026-06-29): the torch-free online path failed this** — it jumbles multiple speakers into one block
      because the VAD emits multi-speaker utterances (segmentation ceiling). Phase-exit now goes through Inc 7.
- [ ] **Inc 7 — pyannote post-meeting refine** (accuracy-max opt-in; torch-free online path stays the default):
  - [x] **7a — Them recorder**: `transcript/recorder.py::ThemAudioRecorder` streams `<folder>/them.wav` when
        `diarization.refine` is on (off by default; raw audio otherwise not retained), recording the meeting-time
        offset for turn→segment mapping; wired into pipeline (Them only) + session. **Done.**
  - [x] **7b — Offline diarizer + pyannote backend** — **Done.** `diarization/offline.py`: `OfflineDiarizer`
        protocol + `SpeakerTurn` + `PyannoteDiarizer` (lazy torch/pyannote; feeds an **in-memory waveform tensor** so
        torchcodec/ffmpeg never decode — confirmed ffmpeg is absent and decoding via file path would fail) using
        `exclusive_speaker_diarization`; `build_offline_diarizer`. Settings `pyannote_model` /
        `hf_token` (SecretStr, defaults to the `hf` CLI login) / `refine_device` (cpu default; mps available).
        Installed `diarization-pyannote` (torch 2.12.1, pyannote.audio 4.0.5) — **`make licenses` + `pip-audit`
        green** over the torch tree (torchcodec shows license UNKNOWN = PyPI-metadata gap, it's BSD-3; the gate is a
        copyleft denylist). **Validated on real audio**: JFK(A)|english(B)|JFK(A) → 2 speakers, both JFK spans →
        same speaker across the gap. +3 seam tests.
  - [x] **7c — Refine orchestration** — **Done.** Pure `order_speakers` + `assign_segment_speaker` (max time-overlap,
        offset-shifted; heavily unit-tested); `SpeakerService.apply_diarization` (atomic: delete online clusters →
        create Speaker 1..N → relabel Them segments); `transcript/refine.py::rediarize_meeting` (read `them.wav` +
        offset sidecar → diarize off-thread → map → relabel → rewrite `transcript.md` via the finalize path); recorder
        now writes a `them.json` offset sidecar for out-of-process runs. CLI `hearsay rediarize <id|latest>`. +7 tests
        (mapping + end-to-end relabel with a stub diarizer + sidecar). **130 pass; full `make ci` green.**
  - [x] **7d — API endpoint + UI button** — **Done.** `POST /api/meetings/{id}/rediarize` (thin: 404 unknown / 409 no
        them.wav / returns the new `Page[SpeakerRead]`; heavy pyannote work stays off-loop) + a "Refine speakers" button
        on the finalized transcript header (`useRediarize`, 10-min client timeout, inline error, invalidates the
        meetings + identities query trees). OpenAPI→TS regenerated; +2 API tests (404/409). web tsc + vite build green.
        Still TBD: them.wav retention/cleanup policy (kept for now so refine is re-runnable; `delete-meeting` removes
        the folder).
  - [x] **7e — On-device validation — PASSED (2026-06-30)**: on a real 2-person call the torch-free online path
        reported **7 speakers**; `hearsay rediarize` (pyannote) corrected the transcript to **2**. This drove the
        decision to make pyannote the default diarization (`diarization.refine` now defaults on). Follow-up:
        auto-refine-at-finalize (currently triggered explicitly via the button / CLI).

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
