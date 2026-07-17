# hearsay — working plan & TODO

Durable, resumable tracker. Check items off as you go. Canonical design doc: the plan at
`~/.claude/plans/i-want-to-plan-keen-lake.md`. IPC contract: `shared/protocol/ipc.md`.
Conventions: `CLAUDE.md`.

## How to resume

**ACTIVE WORK (2026-07-14) — architecture-review remediation, tracked in `docs/remediation-plan.md`.**
WP1-WP6 are done and verified locally but **not yet committed** on `feat/packaging-dmg-uninstall`;
WP7-WP10 remain. WP6 (IPC contract reconciliation) landed the one real correctness bug (transcript
timestamp drift, now silence-padded on gaps), the capture-reader resync + seq-gap logging, the
helper-crash supervisor (an unexpected capture death now finalizes the meeting), the head-of-line fix
(recorder off the ASR-backpressure path), Swift `start_capture` arg validation + ring-overrun→seq, and
the ipc.md reconciliation. See that plan's "Progress (resume here)" block for the exact state,
verification gaps, and what's next (WP7 — Swift helper RT-audio hygiene + watchdog correctness).

**UPDATE (2026-07-14) — the Rust core is canonical; the Python backend has been removed.** The Rust
`hearsay-core` is the shipping artifact; the OpenAPI + IPC-fixture codegen is generated from Rust,
and the Settings page is now backed in the Rust core. Entries below that describe the Python core
(`src/hearsay/`) are historical.

**CURRENT FOCUS (2026-07-01) — cross-platform (macOS + Windows) Rust + Tauri foundation.** Windows is
now a **committed near-term requirement**. Since nothing is shipped yet (POC), the decision (full arc in
[[hearsay-windows-requirement]] memory + `docs/architecture-cross-platform.md`) is to rebuild the
foundation as **ONE Rust + Tauri app** — per-OS only at capture + ASR acceleration (~90% shared),
**local-only** inference, one signed installer per OS. Hard constraints: runs on a **16GB Windows laptop
with integrated graphics** (ref SKU Intel Core Ultra 5 225U) + an **M-series Mac 16GB+**; easily
distributable to non-technical users. Key insight: the Windows-floor design (light streaming model live +
heavy ASR/diarization **offline at stop**) designs away the two problems the FluidAudio/ANE pivot solved
(live-diarization accuracy + Metal contention), so **whisper.cpp works as one unified engine on both
OSes** (it is the revived pre-ANE stack).

**Branch `feat/cross-platform-rust-tauri` (NOT merged to main).** Target design:
`docs/architecture-cross-platform.md`. Crate map: `rust/README.md`.

**STATUS (2026-07-02) — the macOS foundation is BUILT and validated end-to-end in the frontend.** All 8
crates are implemented; the Rust `hearsay-core` binary runs the full live app on the Mac and was confirmed
on-device (user: live captions + diarization "seems fine"; the offline "Refine speakers" pass "looks pretty
good"). The Mac path **reuses the proven Swift stack** (capture via `hearsay-helper`; live streaming +
diarization via the `hearsay-live`/`hearsay-me` FluidAudio sidecars, which the Rust `ProcessTranscriber`
spawns directly — identical stdio protocol) — so the whisper-vs-FluidAudio fork is effectively **taken:
FluidAudio/ANE on Mac** (fast, proven), the Rust whisper path is for **Windows** + the offline refine.
`hearsay-inference` = whisper offline ASR (Mac-verified) + the refine (diarize via Swift `hearsay-diarize`
+ re-transcribe). **All three small Mac follow-ups are DONE 2026-07-02** (auto-refine-at-stop, carry-forward of
locked manual labels, cross-meeting voiceprints) — not yet on-device-validated end-to-end. **The Windows
inference path is also DONE + proven end-to-end on the Mac (2026-07-02):** the pure-Rust engines (sherpa-onnx
streaming ASR live + offline diarizer, whisper offline refine) + the `SherpaTranscriber` adapter run the full
`WavFileSource -> Orchestrator -> SQLite` pipeline with **no Swift** (capstone: real recording -> 7 finalized
transcript segments). Diarization accuracy is bounded (~3 vs FluidAudio's 2; better models are non-commercial —
FluidAudio stays the Mac tier). **The ONLY remaining piece is real capture: a cpal `AudioSource` (WASAPI
loopback Them + mic Me) + a `WindowsBackend` — genuinely needs Windows hardware.** Blow-by-blow in the progress
log below.

**HARDENING + polish (2026-07-13).** A deep review (security / efficiency / dead code / dead tests) landed its
fixes: a media-frame allocation clamp (OOM-DoS on a hostile helper header), backpressure on the live PCM
channels (bounded, not unbounded), `spawn_blocking` for the stop-time WAV/transcript writes, and dead-code
removal. The per-meeting `audio.wav` recorder now **streams to disk** (RAM O(meeting) -> O(seconds); the fragile,
buffer-forcing global peak-normalize dropped — capture-level samples, clamped), and the web player gained a
**0-200% playback volume slider** (Web Audio gain, boosts quiet recordings past what native `<audio>` volume
can). `make ci` green.

**DONE + tested — 80 Rust tests (+ opt-in `--ignored`: jfk ASR, synthetic capture, real-recording refine),
`cargo test` + `clippy -D warnings` + `rustfmt` green, gated by `make ci`:**
- Rust workspace `rust/` (8 crates) + `make rust-{build,test,lint,fmt}` folded into `make ci`.
- **`hearsay-ipc`** — media-frame codec (validated byte-for-byte against `shared/fixtures/frames.jsonl`,
  the same golden vectors Python/Swift check) + NDJSON control channel (Command/Reply/Event, sorted-key wire).
- **`hearsay-attribution`** — voiceprint cosine matching + diarization mapping (`order_speakers`,
  `assign_segment_speaker`), pure logic, zero deps.
- **`hearsay-engine`** — the neutral `LiveEngine` trait seam (meeting lifecycle + live-transcript subscribe)
  + the `DisabledEngine` stub. Consumed by `hearsay-core`, implemented by `hearsay-orchestrator` — a tiny
  lean crate (hearsay-db + async-trait + tokio-sync + uuid) so neither adapter depends on the other (breaks
  the would-be `core -> orchestrator -> core` cycle, and keeps the orchestrator off the axum/web stack).
- **`hearsay-db`** — SQLx 0.9 + SQLite: one forward-only baseline migration (meetings/identities/clusters/
  segments), row types + text enums, pool with the `engine.py` pragmas (WAL/busy_timeout/foreign_keys),
  runtime-checked queries. (Extended with the pagination/join/rename/delete queries `hearsay-core` needs.)
- **`hearsay-core`** — axum 0.8 HTTP/WS API (port of `src/hearsay/api/`). Full self-contained surface:
  meetings/segments/speakers/identities queries + the `{total,page,page_size,items}` envelope, pure-DB
  writes (rename->bind+relabel, delete->DB+folder), audio file serving (Range, query-or-bearer token),
  static UI + token injection + minimal CSP, loopback Host/Origin hardening + per-session bearer token,
  and a **utoipa** OpenAPI doc (`GET /openapi.json` + `--dump-openapi`, for OpenAPI->TS). tracing JSON
  logs; `#[tokio::main]` binds 127.0.0.1 with graceful shutdown. The meeting **lifecycle** (start/stop)
  and the **live transcript WS** sit behind a `LiveEngine` trait seam (mirrors the Python `create_app`
  `SessionManager` injection); the `DisabledEngine` is the fallback, but the binary now wires the real
  `Orchestrator` + macOS `Backend`. `POST /rediarize` now runs the real offline refine (see below).
  Deps (verified-latest via `cargo add`): axum 0.8.9, tower-http 0.7, tokio 1.52, utoipa 5.5, tracing
  + tracing-subscriber, serde/serde_json, async-trait 0.1, sqlx 0.9, uuid, chrono, tower (+ tempfile dev).
  Committed `81c0b9b`.
- **`hearsay-orchestrator`** — the `LiveEngine` implementation (port of `src/hearsay/transcript/` +
  `helper/supervisor.py`). Creates the meeting row + folder, drives an `AudioSource`, routes each stream's
  PCM to its `Transcriber`, and persists + broadcasts the partial/final segments they emit (Them binds
  `Speaker N` clusters; segment times shifted by the stream's first-fed offset; broadcast JSON matches the
  Python `TranscriptEvent` `{kind,stream,speaker_label,text,start_s,end_s}` byte-for-byte). The two external
  backends are behind traits — `AudioSource` (capture) + `Transcriber` (sidecar) — so the lifecycle is tested
  with scripted fakes over in-memory SQLite (9 tests: full route/persist/broadcast/offset, busy guard, stop
  unknown, slugify/folder-name, feed framing + segment parse). Real `tokio::process` `ProcessTranscriber`
  (faithful to `live_base.py` stdio: `<u32 len><f32 pcm>` in, NDJSON segments out) is included for when
  `hearsay-inference` ships the sidecar binaries. The offline refine at stop (auto-refine) is now DONE
  (2026-07-02, via a `Refiner` trait seam — see the progress-log entry). The
  stereo `audio.wav` recorder + the `transcript.md`/`meeting.json` output are also DONE (2026-07-02 entries).
  Also extended the
  `hearsay-core` seam: `LiveError::Internal` / `ApiError::Internal` (500) so a real engine can surface DB
  errors, and `hearsay-db insert_segment` gained a `cluster_id` param (Them finals bind a cluster). Deps:
  tokio (process/io-util/sync/rt/time), async-trait, serde/serde_json, sqlx, uuid, chrono, tracing
  (+ tempfile/tokio-macros dev). Committed `d34453e` (+ tracker fixup `5d1245b`).
  - **Cycle broken (2026-07-02):** the `LiveEngine` trait moved to the new lean `hearsay-engine` crate, so
    `hearsay-orchestrator` now depends on `hearsay-engine` (not `hearsay-core`) — it no longer pulls in the
    axum/tower/utoipa web stack, and `hearsay-core`'s binary can construct the orchestrator without a cycle.
    The final swap (main.rs `DisabledEngine` -> `Orchestrator`) still waits on a real `Backend` (capture +
    inference). Behavior-neutral refactor; 62 tests still green. Committed `d778191`.
  - **Hardening + a hardware-free dev path (2026-07-02):** added a `WavFileSource` (a file-backed
    `AudioSource`: reads the stereo 16 kHz `audio.wav`, Me=L/Them=R, streams timed chunks, holds open until
    stop; `hound` 3.5.1, Apache-2.0) so the whole real pipeline runs end-to-end from a recording without
    capture hardware. Plus a `mock_sidecar` fixture bin (speaks the real `<u32 len><f32 pcm>`-in / NDJSON-out
    contract) driving 3 new integration tests: `ProcessTranscriber` spawn/feed/drain end-to-end (was only
    codec-unit-tested), `WavFileSource` stereo framing, and a **capstone** running a WAV through two real
    `ProcessTranscriber` sidecars into SQLite (only the device + model are stand-ins). 65 tests green.

**CRATES 7-8 (in progress):**
- **`hearsay-capture` (macOS) — DONE (2026-07-02).** `SwiftHelperSource` implements the orchestrator's
  `AudioSource` by driving the proven Swift `hearsay-helper` over the `hearsay-ipc` sockets (bind control +
  media, spawn `serve --socket-dir [--synthetic]`, handshake hello + `start_capture`, pump 28-byte media
  frames -> `CaptureChunk`s). Verified end-to-end against the real helper `--synthetic` (Me + Them frames).
  **The Rust core is now wired to run live on the Mac** (`hearsay-core` `main.rs` = `Orchestrator` +
  `MacBackend`: `SwiftHelperSource` + `ProcessTranscriber`s spawning the built `hearsay-live`/`hearsay-me`
  FluidAudio sidecars — identical stdio protocol, so live streaming + diarization reuse the Swift stack).
  Smoke-tested: `hearsay-core --synthetic` -> start meeting (helper spawns) -> stop -> finalized. `web/dist`
  built + FluidAudio models cached, so it's frontend-ready. Windows cpal capture is the remaining half of the
  trait. **VALIDATED on-device in the frontend (2026-07-02, user: "seems fine")** — live captions +
  diarization work in the browser through the Rust core. The macOS cross-platform stack is proven end-to-end.
- **`hearsay-inference`** — whisper.cpp + Silero VAD + offline diarization, tiered models. The big one;
  provides the orchestrator's `Transcriber` (the sidecar binaries `ProcessTranscriber` spawns) + the offline
  refine. **Started (2026-07-02) — the offline ASR slice is DONE + verified on the Mac.** `whisper-rs` 0.16
  (Unlicense) + `hound`: `WhisperAsr::load(ggml)` + `transcribe(&[f32]) -> Vec<AsrSegment>` (16 kHz mono,
  centisecond bounds), a `read_wav_mono_16k` helper, and a `hearsay-inference <model> <wav>` CLI (the manual
  accuracy tool). CPU by default (portable); `metal`/`vulkan`/`cuda` are opt-in Cargo features forwarding to
  `whisper-rs` — the per-OS accel is a build flag, not a fork. Verified: `outputs/models/ggml-base.bin
  outputs/jfk.wav` -> verbatim JFK quote at ~50x RT (CPU); `ggml-large-v3-turbo` + `--features metal` ->
  verbatim at ~25x RT. Models live in `outputs/models/` (gitignored: ggml-base/large-v3-turbo/large-v3 +
  silero_vad.onnx + wespeaker CAM++). Tests: +2 unit (WAV downmix, reject-non-16k) in the gate; an
  `#[ignore]`d `transcribes_jfk_clip` smoke (opt-in, needs the gitignored model). **The offline REFINE is also
  DONE + wired + validated on-device** (`refine_them` = re-diarize the Them track via the Swift
  `hearsay-diarize` sidecar + re-transcribe each turn with whisper; wired to `POST /rediarize`; see its
  progress-log entry). **NEXT slices (all for the Windows path — Mac uses the Swift sidecars):** a pure-Rust
  streaming `Transcriber` (Silero VAD ONNX + streaming ASR + pure-Rust Segmenter) + a pure-Rust offline
  diarizer (Silero + wespeaker/pyannote ONNX via `ort`) so non-Mac needs no Swift; + WER/DER scoring vs
  references. NB: adding `whisper-rs` means `make rust-{build,test,lint}` now compiles whisper.cpp (cmake + C++)
  — ~15 s cold, cached after; needs `cmake` + a C++ toolchain (present via Xcode CLT). Cosmetic: whisper.cpp
  logs some lines to stderr (a `whisper-rs` log hook can silence it later).
- **Tauri shell — SHIPPED (2026-07-14).** `web/src-tauri/` hosts the React UI, bundles + spawns
  `hearsay-core` + the Swift sidecars (`externalBin`), and points the window at the core's loopback URL;
  `make dmg` builds the ad-hoc-signed `.app` + `.dmg` (unsigned/un-notarized by design — no Apple
  Developer account; see `docs/packaging.md`). Remaining: real Developer-ID signing + notarization.

**MODEL VERIFICATION (corrected 2026-07-02 — NOT a build gate; it is Mac-doable work).** The earlier
framing wrongly treated a *Windows-floor* determination as a hard gate on building `hearsay-inference`. It
is not: **model accuracy (WER/DER) + memory are hardware-independent and verifiable on the Mac now**, and
building the inference path (whisper.cpp on Metal + ONNX) is exactly how you run that verification — you
can't choose model tiers without running the models. Split the two concerns:
- **Accuracy / model choice (do it on the Mac, now):** build the whisper.cpp/ONNX path, transcribe known
  audio on Metal, score WER (ASR) + DER (diarization), pick the tiers. This also resolves the one design
  fork — **unify on ONE whisper.cpp/ONNX family both platforms, vs keep FluidAudio/ANE as a macOS
  high-accuracy tier** — from real numbers. Bias to the lighter end (zipformer-live + turbo/distil-large
  refine).
- **225U real-time throughput (genuinely deferred):** whether the chosen models hold real-time on the
  Windows floor's iGPU. Needs the target (a borrowed 225U or **Intel Tiber AI Cloud**'s free Core Ultra),
  or the quick **Buzz** (whisper.cpp + Vulkan) timing check. A confirmation of a Mac-made choice, not a
  blocker for it.

**Resume the Rust work:** `. "$HOME/.cargo/env"` first (the Bash-tool shell does not auto-source it);
`make rust-test` / `rust-lint` / `rust-fmt`; per-crate `cargo test --manifest-path rust/crates/<crate>/Cargo.toml`.
Pin new deps via `cargo add` (verified-latest, never guess). **The macOS foundation is DONE + validated
end-to-end** (see the STATUS block up top + the progress log): all 8 crates implemented; `hearsay-core`'s
binary runs the full live app on the Mac (real capture + streaming captions + live diarization + the offline
"Refine speakers" pass), reusing the Swift `hearsay-helper` + FluidAudio sidecars. Run it: from the repo root,
`cargo build --manifest-path rust/Cargo.toml -p hearsay-core` then
`HEARSAY_SERVER_PORT=8799 DATABASE_URL="sqlite://$PWD/outputs/db/hearsay-rust.db" ./rust/target/debug/hearsay-core`
(add `--synthetic` for no-permission plumbing), open the printed `?token=` URL. **NEXT — two directions,
neither blocking the other:** (1) **the Windows path** (the real cross-platform payoff, needs Windows
hardware): `hearsay-capture` cpal (WASAPI loopback Them + mic Me) + a pure-Rust streaming `Transcriber`
(Silero VAD + streaming ASR + Segmenter) + a pure-Rust offline diarizer (ONNX via `ort`), so non-Mac needs no
Swift; (2) **small Mac follow-ups — ALL DONE 2026-07-02** (on-device end-to-end validation still pending):
~~auto-refine-at-stop~~ (a `Refiner` trait seam; `stop_meeting` best-effort refines before writing the
transcript), ~~carry-forward of locked manual labels~~ + ~~cross-meeting voiceprints~~ (both in
`replace_them_segments`, so both refine paths get them). The Windows path is the only remaining cross-platform
work. Full context: [[hearsay-windows-requirement]].

**Prior on-main focus (now paused behind this):** the three-item focus below — items 1+2 (dead-code, WAV
consolidation) merged; **item 3 (post-meeting LLM notes) NOT started** — is paused while the cross-platform
foundation is built. The FluidAudio pivot + post-pivot live-UX features (below) are all done + merged to `main`.

---

**Status (2026-07-01):** The **FluidAudio / Apple-Neural-Engine pivot is COMPLETE and MERGED to `main`** (`--no-ff`
merge `00672ef`; `make ci` green — ruff + mypy --strict (63 files) + 125 pytest + swift selftest + audit + licenses).
The on-device audio-AI moved off torch + whisper.cpp/Metal onto **FluidAudio** (Apache-2.0; diarization + Parakeet
models CC-BY-4.0, **ungated**) on the **Apple Neural Engine**, in Swift sidecars the Python core feeds over stdio:
`hearsay-live` (live Them — streaming diarization + Parakeet), `hearsay-me` (live Me — streaming VAD + Parakeet),
`hearsay-diarize` (post-meeting refine), `hearsay-asr` (Parakeet, used by the refine to re-transcribe each turn). The
**Python core runs no ML models** (numpy only, to pack PCM + build/read the stereo `audio.wav`). Architecture = **B**: the capture binary
(`hearsay-helper`) is a lean PCM streamer; all AI is in the sidecars. The whole arc (F1-F4, steps 2/3a/3b, the VAD tune,
the sidecar-death fix, the docs + CLAUDE.md reconciliation) is in the progress log below. [[hearsay-swift-pivot-direction]]

**Pick up here → the three post-pivot live-UX features are all VALIDATED on-device and MERGED to `main`** (2026-07-01;
playback + streaming-Me via `de7a436`, streaming-Them + finalize fixes via `be8cb8f`). They were built on `main` after
the FluidAudio pivot:
1. **In-browser audio playback** — `MeetingAudioRecorder` writes a mixed `audio.wav`; `GET /api/meetings/{id}/audio`
   serves it; the `TranscriptView` player highlights the current line + seeks on click. **VALIDATED:** the user confirmed
   the player + line-highlight + click-to-seek work and playback "sounded good" (loud + smooth). Objective check on the
   16:55 recording's `audio.wav`: peak **0.900** (the normalize-to-0.9 held), **0.05%** internal zeros, **0** silent gaps
   ≥1ms — vs the pre-fix 0.16 peak / 7.9% zeros / 530 gaps that the peak-normalize + contiguous-write fixes addressed.
2. **Live streaming "Me" captions** — `hearsay-me` uses FluidAudio `StreamingUnifiedAsrManager`; live "Me" text streams
   in as growing partials (dimmed) then snaps to the final. **VALIDATED:** in the 16:55 recording the user narrates the
   test ("...is it actually gonna stream me? ... it does actually stream me. It just doesn't do the other ones") — live
   Me streaming works; Them-not-streaming is the expected deferred follow-up (next), not a regression.

3. **Live streaming "Them" captions** — `hearsay-live` runs a `StreamingUnifiedAsrManager` alongside the existing LS-EEND
   diarizer + batch Parakeet: in-progress (not-yet-finalized) Them audio streams in as growing **partial** transcripts
   (re-anchored to each finalized turn boundary), **speaker-less** (`speaker` -1, broadcast as `Them`) — the diarizer only
   assigns a speaker at turn end, so attribution is deferred to the **final** (the proven per-turn batch path is untouched:
   batch-transcribe each finalized turn → `Speaker N` + cluster). Sidecar emits `{kind:"partial|final", speaker, text,
   start_s, end_s}`; `LiveThemProcessor` broadcasts partials to the UI only, finals unchanged; no frontend change. Shipped
   with two finalize-transcript bug fixes surfaced during validation (both frontend): (a) transcript lines **duplicated**
   after auto-refine — `seed` now replaces the finals on finalize instead of merging; (b) UI **never showed the refined
   transcript** after stop — `/stop`'s inline refine exceeded the 15s fetch timeout, so the client aborted before
   `onSuccess` could invalidate; `useStopMeeting` now uses the 600s `REFINE_TIMEOUT_MS`. **VALIDATED on-device** (user:
   "it's working ok" — streaming works, no dupes, refined transcript loads without a refresh). ANE memory was fine with
   `hearsay-live` holding 3 models. The optional **level meter is DROPPED** (user declined it once Them streams). **MERGED
   to `main`** (`--no-ff` merge `be8cb8f`; `make ci` green — 138 pytest + mypy --strict 64 files + swift selftest + audit +
   licenses; web tsc/build green).

**Next — the user's current focus (three items, 2026-07-01). Items 1 + 2 DONE + merged; RESUME AT ITEM 3.**
1. **Dead-code removal — DONE + MERGED to `main`** (`--no-ff` merge `dfc4fbc`). Removed the vestigial ASR model picker
   (endpoints + schemas + UI + `ASRSettings`/`ASRBackendKind`/`models_dir`/the no-op `--model` flag) and stale
   whisper/mlx/fusion comments; ~520 lines gone. Behavior-neutral (Parakeet was already the only ASR path, ignoring every
   picker knob). `make ci` + `make web-ci` green. Phase-3 placeholder enums (`ActiveSpeakerMode`, `NameHintSource`) kept
   as scaffolding. (Also removed a stray `src/hearsay/vad/` bytecode-only dir — untracked leftover of the deleted VAD pkg.)
2. **Consolidate the WAV files to one — VALIDATED on-device + MERGED to `main`** (`--no-ff` merge `bf2aecb`). `them.wav` +
   mixed-mono `audio.wav` collapsed into ONE timeline-accurate **stereo** `audio.wav` (Me = left, Them = right, normalized
   by the overall peak to 0.9). Playback plays it spatially via a plain `<audio>` (mono devices downmix) — no frontend
   change; the refine reads the Them (right) channel (timeline-anchored, so the offset sidecar + all offset threading are
   gone). `audio.record` is now the single audio-retention switch; `ThemAudioRecorder` deleted. **Validated:** the 20:16
   meeting wrote one 2-channel `audio.wav` (Me=L peak 0.900, Them=R peak 0.482, no `them.wav`), played back, and the
   refine produced correct speakers off the stereo Them channel. `make ci` + `make web-ci` green.
3. **← RESUME HERE. Post-meeting accuracy refinement + note distillation + action-item outcomes** — the Phase-4-sized LLM
   piece. NOT STARTED (no `llm/` code yet; the `bedrock` extra = boto3 exists in `pyproject.toml` but is unused). The
   canonical design is the plan's **Phase 4** (`~/.claude/plans/i-want-to-plan-keen-lake.md`, lines ~98/113/144-149/252,
   and risk #7); the `export/` **Sink seam already anticipates "transcript + notes."** Note: the plan describes a *rolling
   in-meeting* notes pipeline, but the user framed this **post-meeting** (simpler, dodges compute contention — risk #7
   explicitly allows post-meeting notes).

   **Proposed shape (mine, for the next session):**
   - **`llm/` provider layer** — `LLMProvider` protocol + an **OpenAI-compatible** client (local default: Ollama / LM
     Studio / llama.cpp) + **Bedrock Converse** (lazy `boto3`). New `llm` settings group (provider / base_url / model /
     key as `SecretStr`).
   - **Note distillation** — post-meeting, feed the finalized transcript to the LLM → structured result (summary /
     decisions / action_items) → render an atomic `notes.md` via the Sink seam.
   - **Action items** — extracted in that same structured pass.
   - **API + UI + trigger** — endpoint to generate/fetch notes, a UI notes panel, a trigger.

   **OPEN DECISIONS (I put these to the user as an `AskUserQuestion`; they interrupted to wrap up, so they are UNANSWERED —
   ask again before building):**
   a. **LLM provider to build+validate against first** — local OpenAI-compatible (need base_url + model) / AWS Bedrock
      (need region + model id) / build the abstraction with a stub and wire the endpoint later. *(Hard blocker: need a real
      endpoint to validate on-device.)*
   b. **What "post-meeting accuracy refinement" means** — (i) an LLM transcript-cleanup pass (fix ASR mishears /
      punctuation / filler, keeping the original too) vs (ii) it's already covered by the diarize+re-transcribe refine, so
      #3 is really just notes + action items vs (iii) both.
   c. **Action-item storage** — DB-backed structured rows (owner / text / status; checkable in the UI, queryable across
      meetings) + rendered into `notes.md`, vs markdown-only in `notes.md`.
   d. **Trigger** — auto after stop (post-refine) + a "Regenerate notes" button, vs on-demand button only.

**Longer-horizon forks (still open):** Phase 3 (calendar roster + OCR active-speaker naming), Phase 5 (packaging), the
all-Swift-backend decision (revisit now that Python is ML-free), the deferred higher-fi playback (a parallel HQ capture
stream), and the trivial `Them`-partial-label tweak. The pivot is DONE + merged (`main` @ `00672ef`); playback +
streaming-Me merged (`de7a436`); streaming-Them + finalize fixes merged (`be8cb8f`).

**Two carry-over notes for whoever picks this up:** (a) the committed VAD-tune config `VadSegmentationConfig(minSilence
Duration: 0.45, speechPadding: 0.2)` on `main`/earlier commits trips FluidAudio's debug `assert(speechPadding <=
minSpeechDuration)` (default minSpeech 0.15) -> `hearsay-me` crashes on a **debug** build; the streaming-Me rewrite fixed
it (raised minSpeechDuration to 0.2), so it rides in on the merge. (b) Me is NOT re-refined at stop, so switching Me to
StreamingUnified made Me's saved finals StreamingUnified-quality (near-batch, a touch below pure batch) -- acceptable per
the spike, but note it if Me accuracy regresses.
- **(3a) Me → Swift — VALIDATED on-device (2026-07-01).** Ran a real meeting via `serve`: `hearsay-me` (streaming VAD +
  Parakeet) produced "Me" utterances, "working OK." Two caveats the user flagged: (i) turn-end boundaries a little iffy
  when a turn ends with a short/no pause, and (ii) some Parakeet ASR accuracy misses. Root cause of (i): `hearsay-me`
  calls `processStreamingChunk` with **no config**, so it uses FluidAudio's `VadSegmentationConfig.default`
  (`minSilenceDuration=0.75s` — only closes an utterance after 0.75s of silence, so short/no-pause turn-ends don't fire
  promptly; a true no-pause run can only be split by `maxSpeechDuration=14s`). Part of (ii) is the default
  `speechPadding=0.1s` clipping word edges before Parakeet; the rest is Parakeet TDT's quality ceiling. NB: **Me is not
  re-refined at finalize** (only Them's `them.wav` is recorded + re-diarized), so Me's live boundaries are final — the
  "finalizing fixes it" the user saw is the Them refine + the sorted rewrite.
- **(3a-tune) sharpen the `hearsay-me` VAD — VALIDATED on-device 2026-07-01 (user: "test is good").**
  `hearsay-me/main.swift` passes a tuned `VadSegmentationConfig(minSilenceDuration: 0.45, speechPadding: 0.2)` to
  `processStreamingChunk` (both the loop + the flush) instead of the defaults (0.75 / 0.1). Effect: utterances close
  after 0.45s of silence (sharper turn-ends on short pauses) and speech edges get 0.2s of padding (fewer clipped word
  onsets/tails → better Parakeet). Chosen so `minSilence (0.45) > 2x padding (0.2)`, so consecutive utterances can't
  overlap. The user re-ran a real meeting and confirmed the boundaries read well (and the sidecar-death bug fix held —
  no crash). If turns ever over-split mid-sentence, raise `minSilenceDuration` (toward 0.6); if too laggy, lower it
  (toward 0.35). **3a + tuning complete → 3b is next.**
- **(3b) Delete the Silero/`vad/` path — DONE (2026-07-01).** Ripped out the `vad/` package (Silero, Segmenter), the
  pipeline's entire VAD/ASR branch (`_emit`/`_transcribe`/`_context`/`_clean_text`/`_persist` + the `asr`/`vad_factory`/
  `vad`/`broadcaster` params), the `_asr_factory`/`_vad_factory` + `ASRFactory`/`VADFactory` in session, `VADSettings`
  (+ the `me_sidecar` toggle), the `fetch-models` CLI command (Silero was all it did), and the `onnxruntime` dep (pruned
  from `uv.lock`/venv along with flatbuffers + protobuf; its mypy override removed). `TranscriptionPipeline` is now a thin
  router: both streams always go to their sidecars (Them -> `hearsay-live`, Me -> `hearsay-me`); a stream with no processor
  is drained (Them still recorded). `build_asr`/`ParakeetBackend` stay — the **refine** uses them to re-transcribe turns.
  **Python now has zero ML deps** (numpy only, for PCM packing + reading them.wav). Tests: deleted `test_segmenter.py` +
  `test_silero_vad.py`, rewrote `test_pipeline.py` to fake-sidecar routing (+ a no-processor drain test). Docs (README +
  architecture/pipeline/development) updated to drop Silero/fetch-models/me_sidecar. **`make ci` green: ruff + mypy
  --strict (63 files) + 125 pytest + swift selftest + pip-audit + licenses.** NB: step (2) already invalidated stored
  centroids (different vector space) — they re-seed on the next refine.

**CLAUDE.md — reconciled (2026-07-01, DONE).** Updated to the current architecture: the lean Swift capture helper +
FluidAudio/ANE sidecars (`hearsay-{live,me,diarize,asr}`) as their own bullet; the Python core described as
orchestration + speaker attribution + persistence + API, running **no ML models**; `fusion/` removed from the structure
tree + the helper executable list expanded; the "fusion engine" test note replaced with the pure diarization-mapping/
voiceprint helpers; the Speaker-ID layers updated (diarization is Swift/ANE + voiceprints; calendar/active-speaker are
Phase 3); and the Python-3.14 dep-decision note rewritten (the full ML stack moved to Swift, so numpy is the only
ML-adjacent dep). Nothing stale remains.

**Note on the live pipeline shape:** both streams always route to their Swift sidecars (Them -> `hearsay-live`, Me ->
`hearsay-me`); `TranscriptionPipeline` runs no ML and only routes PCM + records them.wav + rewrites the transcript at
stop. `LiveThemProcessor`/`LiveMeProcessor` share a `LiveSidecarProcessor` base (spawn/feed/read/close + broken-pipe
resilience); only `_handle` differs. The live manual-rename relay is gone (sidecars have no bind API) — a rename still
relabels existing segments + is carried forward by the refine.

Also still open (lower priority): the optional both-speakers Me/Them interleave validation; Phases 3 (calendar+OCR), 4
(LLM notes+Bedrock), 5 (packaging). The all-Swift-*backend* question (Python API/DB → Swift too) stays parked as a
Phase-5 decision (revisit once Python is ML-free).

---

**The blocks below (FluidAudio lead, spike numbers, the phased-pivot decision) are now HISTORY** — kept so a fresh
context can see how we got here; the *current* state is the Status line above. A session of experiments (2026-06-30)
settled what does NOT work, so a fresh context can skip re-deriving it:
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
make swift-build                     # build helper + sidecars: hearsay-{helper,diarize,asr,live,me} (skips FluidAudio's broken CLI)
cd web && npm ci && npm run build && cd ..   # build the React UI bundle (web/dist)
make ci                              # clippy + rustfmt + swift selftest + cargo test + codegen drift + audit + licenses
make web-ci                          # web gate: npm ci + OpenAPI->TS drift + tsc + vite build
make rust-serve                      # loopback API + WS + the built UI (prints URL + ?token= link; SYNTHETIC=1 = no permissions)
make dmg                             # build the distributable macOS .app + .dmg (see docs/packaging.md)
```

- Rust core: `rust/crates/`  ·  Swift helper + sidecars: `helper/`  ·  web UI: `web/` (Vite + React + TS)  ·  Tauri shell: `web/src-tauri/`.
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

The historical progress log (completed work, chronological) has moved to
[`TODO-archive.md`](TODO-archive.md) to keep this tracker lean. The active plan continues below.

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
