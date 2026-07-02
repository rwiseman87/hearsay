# hearsay — working plan & TODO

Durable, resumable tracker. Check items off as you go. Canonical design doc: the plan at
`~/.claude/plans/i-want-to-plan-keen-lake.md`. IPC contract: `shared/protocol/ipc.md`.
Conventions: `CLAUDE.md`.

## How to resume

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
+ re-transcribe). **Remaining:** the Windows path (cpal capture + a pure-Rust streaming `Transcriber` +
diarizer, using the whisper harness) — the real cross-platform payoff, needs Windows hardware. **All three
small Mac follow-ups are now DONE 2026-07-02** (auto-refine-at-stop, carry-forward of locked manual labels,
cross-meeting voiceprints) — none yet on-device-validated end-to-end. Blow-by-blow in the progress log below.

**DONE + tested — 71 Rust tests (+ opt-in `--ignored`: jfk ASR, synthetic capture, real-recording refine),
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
- Deferred: the **Tauri shell** (`cargo tauri init`; hosts the React UI; bundler + signing + notarization +
  updater). Validate the `externalBin`-breaks-macOS-notarization bug early (the app is sidecar-based).

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
make sync                            # venv + all deps (Python 3.14; numpy is the only ML-adjacent dep, base)
make swift-build                     # build helper + sidecars: hearsay-{helper,diarize,asr,live,me} (skips FluidAudio's broken CLI)
make ci                              # ruff + mypy --strict + pytest + swift selftest + audit + licenses
cd web && npm ci && npm run build && cd ..   # build the React UI bundle (web/dist)
make web-ci                          # web gate: npm ci + OpenAPI→TS drift + tsc + vite build
uv run hearsay serve                 # loopback API + WS + the built UI (prints URL + ?token= link)
uv run hearsay live --seconds 60     # real pipeline -> live transcripts (on-device validation)
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

- **2026-07-02 (cross-platform Rust — streaming ASR via sherpa-onnx; a clean win).** The other half of the
  pure-Rust inference path, and — unlike the diarizer — it's genuinely good. Built **`StreamingAsr`** in
  `hearsay-inference` wrapping sherpa's `OnlineRecognizer` (streaming zipformer transducer, greedy, endpointing
  on): `load(StreamingModel{encoder,decoder,joiner,tokens})` + `transcribe(&[f32]) -> String`. Downloaded the
  **20M English streaming zipformer** (`sherpa-onnx-streaming-zipformer-en-20M-2023-02-17`, int8) into
  `outputs/models/` — the "light live model" the Windows floor calls for. **Verified on-device** (opt-in
  `--ignored`): the JFK clip -> "...AMERICANS ASK NOT WHAT YOUR COUNTRY CAN DO FOR YOU ASK WHAT YOU CAN DO FOR
  YOUR COUNTRY" (near-verbatim; only "My fellow" -> "UL" garbled), in **0.42 s** for an ~11 s clip (~26x RT, CPU,
  int8). So live captions on the Windows floor are viable, and the offline refine (whisper) re-transcribes at
  stop for the high-quality final anyway. Added a `Streaming` error variant + 1 opt-in test. NB: the 20M model
  has no LICENSE in its tarball — it's an icefall/k2-fsa model (Apache-2.0), but confirm before distribution; a
  larger `en-2023-06-26` model is the accuracy option. **79 gate tests (+1 `#[ignore]`d); clippy + rustfmt
  green.** This slice is batch `transcribe` (WER); NEXT: the partial/final *endpointed* session (is_endpoint +
  reset -> growing partials + finals) wrapped in the orchestrator's `Transcriber` trait, so the Windows live path
  runs without a Swift sidecar (paired with a cpal `AudioSource`, which needs Windows hardware).
- **2026-07-02 (cross-platform Rust — sherpa diarizer DER-tuning; exhausted under the license gate).** Tried to
  close the diarization gap to FluidAudio. Added a `DiarizeTuning` surface (cluster_threshold + min_duration_on/
  off) and swept it on the known 2-speaker clip. Result: with **pyannote-segmentation-3.0** (the *only*
  permissive sherpa segmentation model) + **TitaNet** (which beats CAM++), the pipeline **plateaus at 3 speakers**
  — 0.90/0.95/0.97 all give 3, and raising min_duration_on to 2.0 s (down to 7 turns) still gives 3, so the 3rd
  cluster is high-confidence, not a trimmable blip. It never reaches FluidAudio's clean **2**. The one lever that
  would matter — a better segmentation model — is blocked: sherpa's only better ones are Rev AI's **reverb-
  diarization v1/v2**, both under the **"Rev Model Non-Production License"** (§3.2: research/personal/eval in
  Non-Production only, no commercial use "behind a software layer") — fails the MIT/BSD/Apache gate. **Conclusion:
  no permissively-licensed model set closes the gap.** This firmly settles the fork: **FluidAudio (pyannote
  community-1, ANE) stays the macOS accuracy tier; sherpa-onnx is the cross-platform (Windows) fallback —
  degraded-but-usable (~3 vs 2), default threshold set to 0.9 (best-achievable).** Set `DEFAULT_CLUSTER_THRESHOLD`
  = 0.9 + documented the finding on the const. **79 gate tests; clippy + rustfmt green.** Committed `800686c`'s
  follow-up. NEXT (unchanged options): the streaming-ASR half (`OnlineRecognizer`, independent of this gap), and
  wiring a `Diarizer` seam into `refine_them` (Swift on Mac, sherpa fallback elsewhere).
- **2026-07-02 (cross-platform Rust — pure-Rust offline diarizer via sherpa-onnx; the Windows inference path
  begins).** Chose the base after live vetting: **`sherpa-onnx` crate 1.13.3** (Apache-2.0, first-party k2-fsa,
  actively maintained) — NOT `sherpa-rs` (deprecated/archived) and not raw `ort` (rc-only, no stable, would mean
  hand-building the whole pipeline that already failed here once). One dep gives BOTH deliverables of this path:
  offline diarization (pyannote-seg-3.0 + embedding + FastClustering) + streaming ASR (`OnlineRecognizer`). Its
  `sherpa-onnx-sys` build.rs **downloads a prebuilt static lib** (ureq/tar), so it links in ~16 s on the Mac (no
  giant C++ compile) — the native-dep risk is retired. Built **`SherpaDiarizer`** in `hearsay-inference`
  (`OfflineSpeakerDiarization` + `SpeakerEmbeddingExtractor`): `diarize(&[f32]) -> {turns, embeddings}` — turns
  (1-based ordinal by first appearance) + a per-speaker mean voiceprint (diarization exposes only
  (start,end,speaker), so each speaker's embedding is computed separately, matching FluidAudio's output shape).
  Downloaded the pyannote-segmentation-3.0 ONNX (MIT, non-gated) into `outputs/models/`; reuses the already-present
  wespeaker CAM++ embedding model. **Verified end-to-end on-device** (opt-in `--ignored` tests): the pipeline runs
  (512-dim voiceprints, one per speaker, ~12 s on the miguel-kristina clip). **KEY FINDING (resolves the
  whisper/ONNX-vs-FluidAudio fork with a real number): out-of-box accuracy is NOT competitive** — on the known
  **2-speaker** clip it **over-clusters** (12 speakers at sherpa's default threshold 0.5; a sweep gives 9/9/6/4 at
  0.6/0.7/0.8/0.9). Swapping the embedder **CAM++ -> NeMo TitaNet-small roughly halves it** (6/5/5/5/3 across the
  same sweep) — so the embedder matters a lot — but neither hits 2 in a safe threshold range, vs **FluidAudio's
  clean 2**. **Implication:** keep **FluidAudio as the macOS high-accuracy tier**; sherpa-onnx is the **Windows/
  non-Mac path**, and getting its DER competitive is real tuning work (better segmentation model e.g. pyannote
  community-1 ONNX + embedder + threshold, validated across labeled clips) — pending before it feeds the refine.
  Not yet wired into `refine_them` (that seam is next). Added a `Diarize` error variant + 2 opt-in tests (a
  2-speaker smoke + a threshold/embedder sweep harness). **79 gate tests (+2 `#[ignore]`d); clippy -D warnings +
  rustfmt --check green.** NEXT: DER-tuning the diarizer, and/or the streaming ASR half (`OnlineRecognizer`), and
  wiring a `Diarizer` seam into `refine_them` (Swift on Mac, sherpa elsewhere).
- **2026-07-02 (cross-platform Rust — cross-meeting voiceprints).** The last small Mac follow-up. Turned out
  to be **pure Rust** — the Swift `hearsay-diarize` sidecar *already emits per-speaker embeddings*
  (`speakers:[{speaker, embedding}]`, FluidAudio's mean-of-segments speaker database), and the
  `hearsay_attribution::voiceprint` primitives (`cosine`/`match_identity`/`centroid_{to,from}_bytes`) were
  already built + tested; the Rust refine just ignored the embeddings. Wired them end-to-end: (1)
  **`hearsay-inference`** — `refine_them`/`refine_audio_file` now return `RefineOutput { segments, centroids }`,
  parsing the `speakers` field, mapping each to its 1-based ordinal, and L2-normalizing (`build_centroids` +
  `l2_normalize`, f64 norm matching Python). (2) **`hearsay-db`** — `RefineResult { segments, centroids }`;
  `replace_them_segments` now stores each ordinal's voiceprint on its cluster and **recognizes returning
  speakers** (`recognize_speakers`: match each centroid against `known_voiceprints` — people named + locked in
  *other* meetings, cosine >= 0.6 `RECOGNITION_THRESHOLD`), binding the identity but leaving it **unlocked**
  (provisional; a manual rename still overrides). Precedence: **manual carry-forward (locked) > recognized
  (unlocked) > `Speaker N`**. Ports `refine.py::_recognize_speakers` + `SpeakerService.{known_voiceprints,
  apply_turn_diarization}`. (3) The `Refiner` trait + both callers (`/rediarize` route, `MacRefiner`) thread
  the centroids through. Provisional recognitions never become a *source* voiceprint (only locked clusters are
  `known_voiceprints`), so an auto-recognition error can't propagate across meetings. +4 tests (`l2_normalize`/
  `build_centroids`; `replace_them_segments` stores + recognizes a returning Alice as unlocked-bound;
  `known_voiceprints` exclude/locked/centroid filters), and the `#[ignore]` real-recording refine now asserts
  one voiceprint per speaker. **75 -> 79 Rust tests; clippy -D warnings + rustfmt --check green.** No Swift
  change was needed. Not yet validated on-device (a returning, previously-named speaker being auto-recognized).
- **2026-07-02 (cross-platform Rust — carry-forward of locked manual labels).** A re-diarize (manual
  `/rediarize` or the new auto-refine-at-stop) previously dropped every cluster and rebuilt fresh unlocked
  `Speaker N` clusters, so a manually renamed + locked speaker was **wiped** — a data-loss bug the auto-refine
  made worse (now every stop). Fixed by porting Python `refine.py::_carry_forward_names` +
  `SpeakerService.apply_turn_diarization`'s name-application into **`hearsay_db::replace_them_segments`**
  (the single chokepoint both refine paths call, so both get it atomically): before dropping the old clusters,
  read the prior *locked* bindings (cluster -> identity name) + the old Them segments, then **vote each locked
  name onto the new turn ordinal its old segments most overlap** (reusing `hearsay_attribution::
  assign_segment_speaker` — hence a new hearsay-db -> hearsay-attribution dep, pure/zero-dep), one name <-> one
  ordinal by highest vote. On rebuild, a carried ordinal is re-bound + **re-locked** to its identity and its
  segments keep the name; everything else stays a fresh unlocked `Speaker N`. So the name follows the speaker
  by **audio overlap**, not by ordinal number (a re-diarize can reorder speakers). Also added an **empty-refine
  guard**: `replace_them_segments(&[])` is now a no-op (never wipe the transcript when the diarizer/ASR yields
  nothing — matches Python's "leaving transcript as-is"; important now that refine runs automatically at stop).
  Extracted a shared `get_or_create_identity` helper (rename + carry-forward both use it; identities are reused,
  not duplicated). +2 hearsay-db tests (carry-forward follows overlap onto a *different* ordinal + reuses the
  identity; empty is a no-op). **73 -> 75 Rust tests; clippy -D warnings + rustfmt --check green.** Not yet
  validated on-device. Voiceprints (the diarizer emitting embeddings + cross-meeting recognition) is the last
  open Mac follow-up.
- **2026-07-02 (cross-platform Rust — auto-refine-at-stop).** Wired the offline refine to run automatically
  when a meeting stops (Python `SessionManager._maybe_auto_refine`), so the Rust core now matches the Python
  behavior the frontend already expects (`useStopMeeting` uses the 600s `REFINE_TIMEOUT_MS` precisely because
  stop auto-refines — no frontend change). Design: since `hearsay-orchestrator` is deliberately ML-dep-free
  (whisper.cpp/cmake stays out; the lifecycle is fake-tested), auto-refine goes through a new **`Refiner`
  trait seam** (sibling of `Backend`): the orchestrator calls it, the real impl lives in the `hearsay-core`
  binary. `Orchestrator::stop_meeting` now runs `maybe_auto_refine` after finalize + before the transcript
  write — **best-effort**: skipped (logged) when no `audio.wav` (retention off) or no refiner, and a refine
  error is logged + swallowed (the live finals stay as the transcript), so a refine failure never fails the
  stop. On success it `replace_them_segments` and the transcript reflects the refined speakers. The manual
  `/rediarize` route is unchanged (still validated on-device); to avoid two ASR code paths both callers now
  share one **`hearsay_inference::refine_audio_file`** entry point (read Them channel + load whisper +
  `refine_them`). New `MacRefiner` (in `main.rs`, alongside `MacBackend`) wraps it via `spawn_blocking`;
  wired into the orchestrator when the new `HEARSAY_AUTO_REFINE` setting (default on) is set. `RefinedThemSegment`
  gained `Clone`; a `ScriptedRefiner` fake (+ call counter, success/failing modes) drives 2 new lifecycle
  tests (auto-refine replaces the live Them segments; a refine error keeps them + the stop still finalizes).
  **71 -> 73 Rust tests; `cargo test` + `clippy -D warnings` + `rustfmt --check` green.** NB: not yet
  validated on-device (a real stop should auto-refine); the two remaining Mac follow-ups are cross-meeting
  voiceprints + carry-forward of locked manual labels. NEXT: on-device validation, those two follow-ups, or
  the Windows path.
- **2026-07-02 (cross-platform Rust — offline refine, wired to `POST /rediarize`).** Built the post-meeting
  refine (Python `refine.py` core) in `hearsay-inference`: `read_them_channel` (right channel of the stereo
  `audio.wav`) + `refine_them(asr, hearsay-diarize, samples)` — write the Them track to a temp wav, run the
  Swift `hearsay-diarize` FluidAudio sidecar (offline speaker turns; reuses the proven diarizer like the live
  path reuses the sidecars), map labels -> `Speaker N` by first appearance, re-transcribe each turn with
  whisper. **Validated on a real recording** (opt-in `--features metal --ignored`): the miguel-kristina clip
  -> 16 segments / 4 speakers, coherent large-v3-turbo text, ~11s. Then **wired it into the app**:
  `hearsay-db replace_them_segments` (transactional: drop Them segments + clusters, keep Me, one unlocked
  cluster per ordinal; +test), `hearsay-orchestrator::write_meeting_files` made pub (rewrite transcript.md
  after refine), `Settings.refine_model` (`HEARSAY_REFINE_MODEL`, default ggml-large-v3-turbo), and the
  `POST /api/meetings/{id}/rediarize` route now runs the refine (spawn_blocking) + returns the refreshed
  speakers -> **the frontend "Refine speakers" button works on the Mac — VALIDATED on-device (2026-07-02,
  user: "looks pretty good").** **71 Rust tests; clippy + rustfmt green.** Deferred: auto-refine-at-stop (this is the manual button; the orchestrator `// TODO(refine)`
  stays), cross-meeting voiceprints (the diarizer returns embeddings; `hearsay-attribution` has the cosine
  matching ready), and carry-forward of locked manual labels. NEXT: the Windows path (cpal capture +
  pure-Rust streaming `Transcriber` + diarizer), or auto-refine-at-stop.
- **2026-07-02 (cross-platform Rust — the macOS live path runs end-to-end through the Rust core).** User
  chose "full streaming + diarization, testable in the frontend." Key insight: the Rust `ProcessTranscriber`
  already speaks the exact stdio protocol (`<u32 len><f32 pcm>` in, `{kind,text,start_s,end_s,speaker}` NDJSON
  out) of the existing Swift FluidAudio sidecars (`hearsay-live`/`hearsay-me`, already built), so the fastest
  path is to **reuse the proven Swift capture + sidecars** on the Mac rather than rebuild them in Rust — the
  "FluidAudio as macOS tier" fork side; the Rust whisper is the Windows path + refine. Built **`hearsay-capture`**:
  `SwiftHelperSource` (an `AudioSource`) spawns `hearsay-helper`, binds control+media Unix sockets, handshakes
  (hello + `start_capture`), and pumps media frames via the `hearsay-ipc` codec into `CaptureChunk`s (port of
  supervisor/media_channel/control_channel.py). Verified against the real helper `--synthetic` (Me+Them
  frames). Then **wired the binary**: `hearsay-core` `main.rs` runs the real `Orchestrator` + a `MacBackend`
  (`SwiftHelperSource` + two `ProcessTranscriber`s -> `hearsay-live`/`hearsay-me`), `--synthetic` flag, new
  `helper_path` setting. **Smoke-tested live:** `hearsay-core --synthetic` -> `POST /api/meetings` starts a
  recording (helper spawns) -> `POST /stop` finalizes. `web/dist` is built + FluidAudio models cached, so
  it's ready to test in the browser (grant mic/screen perms, real capture). **70 gate tests + 3 opt-in
  (`--ignored`: jfk transcription, synthetic capture) green; clippy + rustfmt clean.** This is the milestone
  the user asked for: streaming + diarization, testable in the frontend, all through the Rust core. NEXT:
  on-device browser validation (user); then the Windows path (cpal capture + pure-Rust streaming
  `Transcriber` + diarizer) + the offline refine.
- **2026-07-02 (cross-platform Rust — `hearsay-inference` offline ASR, on the Mac).** After the user
  corrected the "gated on a Windows determination" framing (accuracy is hardware-independent + Mac-verifiable;
  only 225U real-time perf is deferred — tracker + `[[hearsay-windows-requirement]]` fixed), started
  `hearsay-inference`. Offline ASR slice: `whisper-rs` 0.16 (Unlicense, verified) + `hound`; `WhisperAsr`
  loads a GGML model and `transcribe(&[f32])` returns timestamped `AsrSegment`s (English greedy, centisecond
  bounds), plus `read_wav_mono_16k` (downmix) and a `hearsay-inference <model> <wav>` CLI (the manual accuracy
  tool). Per-OS accel is opt-in Cargo features (`metal`/`vulkan`/`cuda` -> `whisper-rs`), default CPU/portable
  — proving "where it runs is a feature flag, not a fork." **Verified on the Mac:** `ggml-base.bin` + `jfk.wav`
  -> verbatim JFK quote, ~50x RT (CPU); `ggml-large-v3-turbo` + `--features metal` -> verbatim, ~25x RT.
  whisper.cpp built clean via cmake in ~15 s (needs `cmake` + Xcode CLT). Tests: +2 unit (downmix,
  reject-non-16k) in the gate + an `#[ignore]`d jfk smoke (opt-in; needs the gitignored model). **68 -> 70
  Rust tests; clippy + rustfmt green.** The accuracy harness the user wanted is live. NEXT: diarization
  (Silero VAD + wespeaker/pyannote ONNX) + WER/DER scoring, then the streaming `Transcriber`.
- **2026-07-02 (cross-platform Rust — `transcript.md` + `meeting.json` output).** Ported the
  `LocalMarkdownSink` render (`src/hearsay/export/local_markdown.py`) to a `markdown` module: at stop the
  orchestrator writes the meeting folder's `transcript.md` (`# {title}`, then a `### HH:MM:SS — Speaker`
  header at each speaker change followed by the turn text) + a `meeting.json` (id/title/folder/status/
  started_at/ended_at), both atomic (temp + rename). Rendered from the finalized DB segments (already ordered
  by `start_s`), best-effort (a write failure never fails the stop). **Finalize-only** for now — the Python
  sink's mid-meeting live-append (crash-safety) is deferred (the live transcript is on the WS + in the DB).
  This un-defers the transcript sink (it only needs the finals in the DB). Tests: +1 unit (`hhmmss`); the
  capstone now asserts `transcript.md` (header + `### ` turns + `Speaker 1` + text) and `meeting.json`
  (`"status": "finalized"`). **67 -> 68 Rust tests; clippy + rustfmt green.** With this + the recorder, the
  orchestrator's whole output-folder story (audio.wav + transcript.md + meeting.json) is complete; only the
  inference-gated offline refine remains deferred.
- **2026-07-02 (cross-platform Rust — `audio.wav` recorder in the orchestrator pipeline).** Ported
  `MeetingAudioRecorder` (`src/hearsay/transcript/recorder.py`) to Rust: one timeline-accurate stereo WAV per
  meeting (Me=L / Them=R), each stream placed by meeting time (sample N = t N/16000), written contiguously
  from a per-stream cursor and only re-anchored to `t0_s` past a 0.2 s gap (so clock jitter never punches
  holes), normalized at close to a 0.9 overall peak (a mono downmix stays in range), int16 stereo via `hound`.
  Wired into the pipeline demux (writes each chunk before routing; finalized when capture ends, best-effort).
  The `Orchestrator` records by default with a `with_audio_recording(bool)` toggle (the eventual config wires
  `audio.record` there). This un-defers the recorder (it only needs the PCM the demux already sees — not
  capture/inference) and makes playback (`GET /meetings/{id}/audio`) work end-to-end. Tests (+2 unit: stereo
  timeline + overall-peak normalization, silent-take-writes-nothing) + the capstone now asserts the pipeline
  emits a valid stereo `audio.wav`. **65 -> 67 Rust tests; clippy + rustfmt green.**
- **2026-07-02 (cross-platform Rust — orchestrator hardening + a hardware-free dev path).** Two additions on
  top of `hearsay-orchestrator`: (1) a **`WavFileSource`** — a file-backed `AudioSource` that reads the
  canonical stereo 16 kHz `audio.wav` (Me=L/Them=R), streams it as timed chunks on the shared `host_ts`
  clock, and holds the channel open until stop (mono -> Them). Adds `hound` 3.5.1 (Apache-2.0, verified). It
  lets the whole real pipeline run from a recording with no capture hardware. (2) A **`mock_sidecar`** fixture
  bin (`src/bin/`, std-only) that speaks the real sidecar stdio contract (`<u32 len><f32 pcm>` in, NDJSON
  segments out), used to lift `ProcessTranscriber` from codec-unit-tested to a real spawn/feed/drain
  integration test. New tests (3): `ProcessTranscriber` e2e, `WavFileSource` stereo framing, and a
  **capstone** (`tests/end_to_end.rs`) that drives a WAV through two real `ProcessTranscriber` sidecars into
  SQLite via the `Orchestrator` — only the audio device + the ML model are stand-ins, proving the real
  source + real transcriber + real orchestrator compose + persist. **62 -> 65 Rust tests; clippy + rustfmt
  green.**
- **2026-07-02 (cross-platform Rust — extracted `hearsay-engine`, broke the `core -> orchestrator` cycle;
  branch `feat/cross-platform-rust-tauri`).** Moved the `LiveEngine` trait + `LiveError` + `DisabledEngine`
  out of `hearsay-core` into a new lean crate **`hearsay-engine`** (deps: hearsay-db + async-trait +
  tokio-sync + uuid — no web stack). `hearsay-core` now `pub use`s them from there (external API unchanged),
  and `hearsay-orchestrator` depends on `hearsay-engine` instead of `hearsay-core` — so the orchestrator no
  longer compiles axum/tower/utoipa, and the `hearsay-core` binary can construct the orchestrator without the
  would-be `core -> orchestrator -> core` cycle. This was the one purely-here step the previous entry flagged.
  8th crate; the graph is now cycle-free and the final `main.rs` swap (`DisabledEngine` -> `Orchestrator`)
  only waits on a real `Backend` (capture + inference). Behavior-neutral: **62 Rust tests + clippy + rustfmt
  still green.** Repointed 3 `hearsay-core` imports (lib re-export, `state.rs`, `routes/meetings.rs`) + 3
  `hearsay-orchestrator` imports (`orchestrator.rs`, `error.rs`, `tests/lifecycle.rs`); `routes/ws.rs`
  unchanged (calls the trait-object method, no import needed). NEXT: `hearsay-capture` / `hearsay-inference`
  (both need real hardware / the model decision) are the only substantial work left.
- **2026-07-02 (cross-platform Rust — `hearsay-core` committed + `hearsay-orchestrator` implemented + tested;
  branch `feat/cross-platform-rust-tauri`).** Committed the finished `hearsay-core` (`81c0b9b`), then built the
  5th of 7 crates: **`hearsay-orchestrator`**, the `LiveEngine` implementation (port of
  `src/hearsay/transcript/` `SessionManager` + pipeline + the two sidecar processors, and
  `helper/supervisor.py`). It creates the meeting row + folder (ports `slugify`/`meeting_folder_name`/
  `_default_title`), drives an `AudioSource`, demuxes the capture channel onto a shared epoch clock, routes
  each stream's PCM to its `Transcriber`, and — per stream — broadcasts partials (UI only) and persists +
  broadcasts finals (Me = "Me"; Them = `Speaker N` + a get-or-create per-ordinal cluster), shifting sidecar
  times by the stream's first-fed offset. The broadcast JSON matches the Python `TranscriptEvent`
  (`{kind,stream,speaker_label,text,start_s,end_s}`). **Design:** the two external backends sit behind traits
  (`AudioSource` <- `hearsay-capture`; `Transcriber` <- `hearsay-inference`) built per meeting by a `Backend`
  factory, so the whole lifecycle is verifiable here with scripted fakes (`testing::ScriptedBackend`) over
  in-memory SQLite. Included the real `ProcessTranscriber` (`tokio::process`, faithful to `live_base.py`:
  `<u32 len><f32 pcm>` on stdin, NDJSON segments on stdout) for when the sidecar binaries exist. Active state
  is a std-mutex `Option<ActiveSession>` (sync `active_meeting`/`subscribe`) guarded by an async op-lock
  (serializes start/stop); stop closes the pipeline (source.stop -> tasks drain each transcriber's tail),
  finalizes the row, and works for a non-active id too (matches Python). **Seam extensions:** added
  `LiveError::Internal` + `ApiError::Internal` (500) to `hearsay-core` so a real engine surfaces DB errors,
  and a `cluster_id` param to `hearsay-db insert_segment` (Them finals bind a cluster) — updated the 5 test
  call sites. **Deferred (documented TODOs, gated on capture/inference):** the stereo `audio.wav` recorder,
  the `transcript.md` sink, and the offline refine at stop; finals persist to the DB (the API's source of
  truth) today. New deps pinned to the workspace-common versions: tokio (process/io-util/sync/rt/time),
  async-trait 0.1, serde/serde_json, sqlx 0.9, uuid, chrono, tracing (+ tempfile/tokio-macros dev). **`make
  rust-test` + `rust-lint` (clippy -D warnings) + `rust-fmt --check` all green: 62 Rust tests (9 new —
  6 unit: slugify/folder-name, feed framing, segment parse; 3 integration: full route/persist/broadcast/
  offset lifecycle, busy guard, stop-unknown).** Committed `d34453e` (per the per-crate workflow, same as
  `hearsay-core` earlier this session). **NEXT:** wiring the
  orchestrator into the `hearsay-core` binary (a small refactor to break the `core -> orchestrator -> core`
  cycle — move the `LiveEngine` trait to a shared crate or split the binary out) is the only purely-here step
  left; `hearsay-capture` + `hearsay-inference` need real hardware + the model decision.
- **2026-07-01 (cross-platform Rust — `hearsay-core` implemented + tested; branch `feat/cross-platform-rust-tauri`,
  uncommitted).** 4th of 7 crates. With the user, picked `hearsay-core` over `hearsay-orchestrator` as the next
  verifiable-here crate (self-contained, depends only on the finished `hearsay-db`; unblocks the shared React UI on
  Rust). Ported `src/hearsay/api/` to axum 0.8: `security.rs` (Host/Origin allowlist + constant-time bearer, unit-tested),
  `config.rs` (env-resolved `Settings`), `schema.rs` (serde + utoipa `ToSchema` DTOs + the `{total,page,page_size,items}`
  envelope), `error.rs` (`{detail}` JSON envelope; DB errors -> 500), `state.rs` (`AppState`), and the routers
  (meetings/speakers/audio/ws/web) + the loopback + token middleware. **Key design:** the capture-dependent routes
  (start/stop meeting, live WS) sit behind a `LiveEngine` trait seam — the Rust analogue of Python `create_app`'s injected
  `SessionManager` — with a built-in `DisabledEngine` (503 / clean WS close) so the crate lands complete + tested before
  `hearsay-orchestrator` exists; every read + pure-DB-write + serving route works now. `POST /rediarize` is 404-or-503
  (needs `hearsay-inference`). Added a utoipa OpenAPI doc (`/openapi.json` + `--dump-openapi`, emits OpenAPI 3.1.0) for the
  OpenAPI->TS pipeline. Extended `hearsay-db/queries.rs` with the pagination/count/join(`SpeakerRow`)/`rename_cluster`
  (transactional bind+relabel)/`delete_meeting` queries the API needs (+3 db tests: pagination, join+relabel, cascade
  delete). New deps pinned verified-latest via `cargo add`: axum 0.8.9, tower-http 0.7, tower 0.5, tokio 1.52, utoipa 5.5,
  tracing(+subscriber), serde/serde_json, async-trait 0.1, uuid, chrono, sqlx 0.9 (+ tempfile dev). **`make rust-test` +
  `rust-lint` (clippy -D warnings) + `rust-fmt --check` all green: 53 Rust tests (16 new in `hearsay-core` — 4 unit
  security + 12 tower-`oneshot` integration over in-memory SQLite covering auth 401/host 400/origin 403, list/get/404,
  the page envelope, segment ordering, rename+label resolution, identities, audio token+404, rediarize 404/503, delete
  204->404, start/stop 503, `/openapi.json`).** NB: uncommitted (the repo commits each crate separately — suggested
  message `feat(rust): implement hearsay-core (axum API + utoipa + LiveEngine seam)`); await the user. **NEXT
  verifiable-here crate: `hearsay-orchestrator`** — implement the `LiveEngine` seam (start/stop + live-transcript
  broadcast) over `tokio::process` supervision + PCM routing, testable with fake sidecars.
- **2026-07-01 (session wrap — items 1 + 2 of the user's three-item focus done + merged; item 3 not started).** Dead-code
  cleanup (`dfc4fbc`) and the single-stereo-WAV consolidation (`bf2aecb`) are both validated + merged to `main`; working
  tree clean. Item 3 (post-meeting notes + action items, the Phase-4 LLM piece) is **NOT STARTED** — the design shape +
  the four open decisions (LLM provider, what "accuracy refinement" means, action-item storage, trigger) are captured in
  the "Pick up here" item 3 above; those decisions were put to the user but interrupted unanswered, so **ask them first**
  when resuming. No `llm/` code exists yet. Also removed a stray untracked `src/hearsay/vad/` bytecode dir.
- **2026-07-01 (WAV consolidation VALIDATED on-device + MERGED to `main`).** The 20:16 meeting confirmed the new format:
  one 2-channel `audio.wav` (Me=L peak 0.900, Them=R peak 0.482, no `them.wav`), plays back, and the refine produced
  correct speakers (`Speaker 1` + `Me`) off the stereo Them channel. Merged `feat/single-wav` -> `main` (`--no-ff`,
  `bf2aecb`). Item 2 of 3 done. **NEXT: item 3 — post-meeting notes + action items (Phase-4-sized; needs a design pass).**
- **2026-07-01 (WAV consolidation — one stereo `audio.wav` replaces them.wav + mixed-mono audio.wav; branch
  `feat/single-wav`, code-complete, needs on-device validation).** Second of the user's three focus items. Collapsed the
  two per-meeting WAVs into ONE timeline-accurate **stereo** `audio.wav` (Me = left channel, Them = right), normalized by
  the overall peak to 0.9 (so neither channel nor a mono downmix clips). `MeetingAudioRecorder` now holds a 2-row buffer
  and writes 2-channel WAV; new `read_them_channel()` reads the right channel for the refine (mono fallback for older
  recordings). Deleted `ThemAudioRecorder`, the `.json` offset sidecar, and `read_offset_s` — the Them channel is
  timeline-anchored, so turn times are already meeting time and all offset threading (`_transcribe_turns`,
  `_carry_forward_names`, the `assign_segment_speaker` offset arg) drops to 0. Playback is unchanged in the frontend: a
  plain `<audio>` plays the stereo file spatially (mono devices downmix). Settings: **`audio.record` is now the single
  audio-retention switch** (feeds both playback and the refine; with it off there's no recording so playback + refine are
  gone), per the user's choice; `diarization.refine`/`auto_refine` still gate whether the refine runs. Reworked
  `test_recorder.py` (stereo channels, timeline anchoring, normalization, `read_them_channel` + mono fallback),
  `test_pipeline.py` (one recorder fed both streams), `test_refine.py` / `test_auto_refine.py` / `test_api.py`
  (them.wav -> stereo audio.wav); updated README + `docs/{api,architecture,development,pipeline}.md`. **`make ci` green
  (ruff + mypy --strict 62 files + 133 pytest + swift selftest + audit + licenses); `make web-ci` green** (one OpenAPI
  drift from the rediarize 409 docstring, regenerated + committed). Net ~54 lines removed. **NEXT: on-device validation
  (playback L/R + the refine off the stereo Them channel), then merge; then item 3 (post-meeting notes/action-items).**
- **2026-07-01 (dead-code cleanup — removed the vestigial ASR picker + pivot remnants; branch `chore/dead-code-cleanup`,
  `85616dc`).** First of the user's three new focus items. Audited with vulture + grep: the pivot cleanup was already
  thorough, but the **ASR model picker** was fully vestigial (Parakeet-only in the `hearsay-asr` sidecar with one bundled
  model, so `ParakeetBackend` ignores every knob). Removed the whole feature and the now-dead config around it: `api/asr.py`
  + `schemas/asr.py` (GET/PUT routes, ASRStatus/ASRSelect/ModelInfoRead), the frontend `ModelPicker` + `useAsrStatus`/
  `useSetAsrModel` + `.model-picker` CSS + dead type re-exports, `ASRSettings` (backend/model/language/beam_size/
  condition_on_previous_text/context_reset_gap_s — all whisper-era, **zero reads**), the `asr` settings group, the unread
  `models_dir` setting, the `ASRBackendKind` enum, `available_models()`/`ModelInfo`, the never-passed `language`/`prompt`
  params on `ASRBackend.transcribe`, and the no-op `hearsay live --model` flag. Fixed stale whisper/mlx/**fusion** comments
  in `asr/base.py`, `parakeet_backend.py`, `services/speakers.py`, `models/segment.py`, `capture_debug.py`; updated README
  + `docs/{api,architecture,development}.md`; regenerated OpenAPI + web TS. **Net ~520 lines removed. `make ci` green (62
  mypy files, 135 pytest, swift selftest, audit, licenses); `make web-ci` green (codegen drift clean, tsc + vite build);
  vulture (conf 80) clean.** Kept `ActiveSpeakerMode`/`NameHintSource` as Phase-3 scaffolding (user's call). Behavior-neutral
  — no on-device validation needed. NB: a stray 1.4 GB `Archive.zip` was found untracked in the repo root (not created by
  this work; left for the user, not committed). **NEXT: WAV consolidation, then post-meeting notes/action-items.**
- **2026-07-01 (Them streaming + the two finalize fixes VALIDATED on-device + MERGED to `main`).** User confirmed "it's
  working ok" — live Them captions stream in speaker-less then snap to `Speaker N` at turn end, no post-stop duplication,
  and the refined transcript now loads without a manual refresh. ANE was fine with `hearsay-live` holding 3 models
  (LS-EEND + batch Parakeet + streaming Parakeet). Merged `feat/them-streaming` -> `main` (`--no-ff`, merge `be8cb8f`):
  the streaming-Them feature (`fa8b5f9`) + the two frontend bug fixes surfaced during validation — the auto-refine
  duplication (`02554e0`, replace-on-finalize) and the stop-timeout (`ca3f6ae`, 600s `REFINE_TIMEOUT_MS` so the inline
  refine finishes before the request aborts). Level meter dropped per the user. **NEXT: nothing in flight — only the
  longer-horizon forks remain (Phase 3 calendar/OCR, Phase 4 LLM notes, Phase 5 packaging, the all-Swift-backend
  decision, higher-fi playback).** See [[hearsay-swift-pivot-direction]].
- **2026-07-01 (bug fix — UI never showed the refined transcript after stop; frontend-only).** The user reported the
  transcript "never updates once I hit stop" (stuck on the live/pre-refine version). Diagnosed from the DB: the meeting's
  segments **were** refined server-side (14 rows, Speaker 1/2, matching `transcript.md`), so the backend + segments API
  were correct -- the UI just never refetched. Root cause: `/stop` now runs the inline auto-refine (re-diarize +
  re-transcribe every turn; a **cold Parakeet sidecar load alone is ~11s**), routinely exceeding the fetch wrapper's
  **15s `DEFAULT_TIMEOUT_MS`**. When it did, the client `AbortSignal` aborted the request -> the `useStopMeeting` mutation
  errored -> `onSuccess` never fired -> the segments query was never invalidated, so the UI kept the WS-accumulated live
  finals. (Meanwhile `useMeetings`' 5s poll flips status to finalized, so my earlier replace-on-finalize dedup fix shows
  the stale live segments -- "no dupes, but never refined." The original "dupes until refresh" was the faster meetings
  where `/stop` returned within 15s.) Fix (`web/src/api/hooks.ts`): give `useStopMeeting` the same 600s timeout the manual
  refine uses (hoisted to a shared `REFINE_TIMEOUT_MS`), so `onSuccess` fires after the refine and invalidates the queries
  -> refined transcript loads without a refresh. Considered a "refetch on recording->finalized transition" safety net but
  **dropped it**: `stop_meeting` commits `status=finalized` **before** running the refine, so that transition fires
  mid-refine and would fetch pre-refine data -- the mutation's `onSuccess` is the only correct "refine done" signal. tsc +
  vite build green. Committed on `feat/them-streaming` (`ca3f6ae`). **Rebuild `web/dist` + restart `serve` to pick it up.**
- **2026-07-01 (bug fix — transcript lines duplicated after auto-refine at finalize; frontend-only).** On stop, the
  auto-refine replaces the Them segments server-side with new per-turn rows at different `start_s` keys; the transcript
  reducer's `seed` action **merged** the refetched DB segments into `state.finals`, which still held the stale live-WS
  finals (old keys) -- so both rendered until a manual refresh reset the state. Fix (`web/src/hooks/useTranscript.ts`):
  `seed` now **replaces** the finals map (and clears partials) when the meeting is finalized (`replace: !isLive`) -- the
  DB is authoritative post-refine -- while still merging during recording (a stale DB fetch can lag the live socket).
  Pre-existing since auto-refine landed; surfaced during Them-streaming validation. tsc + vite build green; no frontend
  test runner in the project. Committed on `feat/them-streaming` (`02554e0`).
- **2026-07-01 (feature: live streaming "Them" captions — code-complete, `make ci`-green, needs on-device validation).**
  Added a `StreamingUnifiedAsrManager` to `hearsay-live` (mirroring the streaming-Me rewrite) **alongside** the existing
  LS-EEND diarizer + batch Parakeet, so live Them text streams in as growing **partials** while the proven per-turn
  finals path stays untouched. Design = purely additive (lowest risk, keeps the validated diarizer-turn finals): the
  streaming ASR transcribes the in-progress (not-yet-finalized) audio; on each finalized turn the diarizer boundary
  re-anchors the streaming context (`reset()` + re-feed from the boundary) so the partial only ever reflects the current
  turn. Partials are **speaker-less** (`speaker` -1) because the diarizer assigns a speaker only at turn end — attribution
  deferred to the final (user OK'd). Sidecar stdout is now `{kind:"partial|final", speaker, text, start_s, end_s}`;
  `LiveThemProcessor._handle` branches: partials broadcast to the UI only (labeled `Them`, not persisted), finals keep the
  Speaker-N + cluster + persist path. **No frontend change** — the reducer already keeps one dimmed partial per stream and
  supersedes it with the stream's next final (leftover from the Me path). Also updated `docs/{pipeline,architecture}.md`
  (Them path now describes partials + finals). +1 test (`test_partials_broadcast_but_not_persisted`). **`make ci` green:
  ruff + mypy --strict (64 files) + 138 pytest + swift build + selftest + audit + licenses.** Committed on
  `feat/them-streaming` (`fa8b5f9`). NB during the build: a stale incremental-build state made the first `make swift-build`
  skip `hearsay-live` (only rebuilt `hearsay-me`) — a `touch` + targeted rebuild fixed it and it compiles clean; watch for
  this if a sidecar edit seems not to take. **NEXT: on-device — `serve` + a meeting, watch Them stream live then snap to
  `Speaker N`; watch ANE memory with 3 models in `hearsay-live`. Then merge to `main`.** The optional level meter is
  **dropped** (user declined it once Them streams). See [[hearsay-swift-pivot-direction]].
- **2026-07-01 (both post-pivot features VALIDATED on-device + MERGED to `main`).** Validated the two features stacked on
  `feat/audio-playback` and merged the branch to `main` (`--no-ff`, merge `de7a436`). **(1) Live streaming "Me"
  captions** — validated by the user's own words on the 16:55 real meeting ("...is it actually gonna stream me? ... it
  does actually stream me. It just doesn't do the other ones"): live Me partials stream word-by-word; Them not streaming
  live is the expected deferred follow-up, not a regression. **(2) In-browser audio playback** — the user confirmed the
  player + line-highlight + click-to-seek and that playback "sounded good" (loud + smooth); objectively confirmed the
  earlier choppy/quiet fixes held on the 16:55 `audio.wav`: peak **0.900** (peak-normalize-to-0.9), **0.05%** internal
  zeros, **0** silent gaps ≥1ms (vs the pre-fix 0.16 peak / 7.9% zeros / 530 gaps). Merged without re-running `make ci`
  (per the user — tree was clean and ci was green at commit time). **NEXT: nothing in flight — the Them-streaming
  follow-up + the optional level meter are the near-term picks; Phases 3/4/5 + the all-Swift-backend decision stay
  parked.** See [[hearsay-swift-pivot-direction]].
- **2026-07-01 (feature: live streaming "Me" captions — code-complete, smoke-tested, needs on-device validation).**
  Rewrote the `hearsay-me` sidecar to use FluidAudio's **`StreamingUnifiedAsrManager`** (Parakeet streaming) instead of
  batch `AsrManager`: the VAD still marks utterance boundaries, but the ASR now emits growing **partial** transcripts as
  you speak (`getPartialTranscript()` per fed chunk) and a **final** at the utterance end (`finish()`). Chosen after a
  spike A/B (`outputs/fluidaudio-spike`): StreamingUnified gives partials + near-batch finals from ONE engine (42x RT,
  61 partials on the twitch clip) vs Nemotron/EOU which were weaker — so no need for a two-engine tandem. `LiveMeProcessor`
  now forwards `kind:"partial"` (broadcast only) vs `kind:"final"` (persist + append + broadcast); the frontend already
  renders + supersedes partials (leftover from the whisper VAD path), so **no frontend change**. Smoke-tested by framing a
  recorded clip into the sidecar: 70 growing partials + 1 final, clean exit. +1 test (`test_partials_broadcast_but_not_
  persisted`). **`make ci` green (137 tests, 64 mypy files, swift build + selftest).** NB: found + fixed a latent bug --
  the tuned VAD config (`speechPadding 0.2` with default `minSpeechDuration 0.15`) trips FluidAudio's debug
  `assert(speechPadding <= minSpeechDuration)`, crashing hearsay-me on a **debug** build; fixed by raising
  `minSpeechDuration` to 0.2. (The *committed* VAD-tune config on `feat/audio-playback`/`main` has the same latent assert
  -- only survives in release, where asserts compile out.) **NEXT: on-device -- run a meeting and watch live word-by-word
  captions for your own voice.** Me isn't re-refined at stop, so Me finals are now StreamingUnified-quality (near-batch);
  Them streaming (speaker-less-until-turn-end) is the follow-up.
- **2026-07-01 (feature: in-browser audio playback with synced transcript highlighting — code-complete, needs on-device
  validation).** The pivot's first post-merge feature (user-requested). New `MeetingAudioRecorder` records one
  timeline-accurate mixed (Me+Them) `audio.wav` per meeting -- both streams are placed by meeting time and summed on
  overlap, so sample N = second N (gaps + leading offset silence-padded); gated on the new `audio.record` setting
  (default on). Served by `GET /api/meetings/{id}/audio` (Starlette `FileResponse` -> Range/`206` for seeking; auth via
  `?token=` query param since an `<audio>` element can't set headers, or a bearer header). Frontend: `TranscriptView`
  renders an `<audio controls>` for finalized meetings and highlights the current line as playback advances
  (`.line--active`, auto-scrolled), click-a-line-to-seek. them.wav (Them-only, for the refine) is unchanged. +8 tests
  (recorder mixing/timeline/padding + endpoint query-token/bearer/range/404); OpenAPI + TS types regenerated; `make ci`
  green (64 mypy files, 134 pytest) + web tsc/build green. **Also, per user feedback (a hard rule): moved numpy to a
  BASE dependency and hoisted every in-method `import numpy` to module level** (recorder, live_base, refine, offline,
  parakeet_backend); dropped the now-empty `asr` extra + the last `--extra asr`/`fetch-models` doc remnants. See
  [[no-imports-in-functions]]. **Validated functional on-device (player + highlight + seek work). User flagged the
  playback was "very bad quality"; diagnosed from the recorded WAV -- not clipping (0%), just very quiet (peak 0.16,
  RMS 0.016; the raw them.wav is equally quiet, so it's a low capture level). Fixed: `MeetingAudioRecorder.close` now
  peak-normalizes the mix to 0.9 (~5.5x on that meeting). User then reported it was **choppy** -- diagnosed from the
  WAV: audio.wav had **7.9% internal zeros / 530 silent gaps (up to 11 ms)** while them.wav was smooth (0.1%). Cause:
  placing every chunk by its own `round(t0_s*rate)` scatters gaps because host_ts jitters a few ms/chunk (the total
  span still matched them.wav, so it's zero-mean jitter, not drift). Fix: `MeetingAudioRecorder.write` now takes the
  `stream` and writes each stream **contiguously** (like them.wav), re-anchoring to t0_s only when it diverges past
  `_RESYNC_GAP` (0.2 s = a real delivery gap). NEXT: record a fresh meeting to confirm smooth + audible playback.
  Remaining ceiling: capture is 16 kHz mono (resampled for the ASR sidecars in `Resampler.swift`), so playback is
  telephone-band. Higher fidelity would need the helper to capture a **parallel higher-rate track** just for playback
  (a new IPC stream + Swift capture-graph change; the 16 kHz ASR path stays untouched). Offered 48 kHz-stereo / 32 kHz /
  16 kHz-stereo-only -- **user deferred it 2026-07-01 ("just leave it for now")**, so 16 kHz mono stands.**
- **2026-07-01 (3b — deleted the Silero/`vad/` fallback; Python is now ML-dep-free).** With 3a validated, removed the
  entire Python VAD/ASR live path: deleted the `vad/` package (Silero, Segmenter), gutted `TranscriptionPipeline` down to
  a thin router (dropped `_emit`/`_transcribe`/`_context`/`_clean_text`/`_persist` + the `asr`/`vad_factory`/`vad`/
  `broadcaster` params -- both streams always route to their sidecars now, a processor-less stream is drained),
  removed session's `_asr_factory`/`_vad_factory`/`ASRFactory`/`VADFactory`, `VADSettings` (+ the `me_sidecar` toggle),
  the `fetch-models` CLI command, and the `onnxruntime` dep (pruned from `uv.lock`/venv with flatbuffers + protobuf; mypy
  override gone). `build_asr`/`ParakeetBackend` stay -- the post-meeting refine still uses them to re-transcribe each
  diarizer turn. Tests: deleted `test_segmenter.py` + `test_silero_vad.py`, rewrote `test_pipeline.py` to fake-sidecar
  routing (+ a no-processor drain test). Docs (README + architecture/pipeline/development) updated to drop Silero/
  fetch-models/me_sidecar. **`make ci` green: ruff + mypy --strict (63 files) + 125 pytest + swift selftest + pip-audit +
  licenses.** End state: the Python core runs **no ML models** (numpy only, to pack PCM + read them.wav); all audio-AI is
  in the Swift sidecars on the ANE. **NEXT: a CLAUDE.md cleanup pass, then merge `feat/fluidaudio-pivot` to `main`.** See
  [[hearsay-swift-pivot-direction]].
- **2026-07-01 (bug fix — live pipeline survives a sidecar dying mid-meeting).** During the 3a-tune re-test the user
  refreshed the browser mid-recording; the tap watchdog rebuilt and a live sidecar (`hearsay-me`/`hearsay-live`) died,
  so its stdin pipe broke. `LiveSidecarProcessor.feed` had no error handling around `stdin.drain()`, so the `_consume`
  task died with `BrokenPipeError`; at stop, `pipeline.close()`'s `await task` (under `suppress(CancelledError)` **only**)
  re-raised it -> `POST /stop` 500'd and the meeting was left **stuck** (finalize never ran, `self._active` never cleared,
  helper left running -> no new meeting could start). Fix (Python-only, no Swift rebuild): (1) `feed()` now catches
  `(BrokenPipeError, ConnectionResetError)`, sets `self._broken`, logs once, no-ops further feeds (mirrors
  `ParakeetBackend`); (2) `pipeline.close()` awaits the consume tasks defensively (log + continue on any non-Cancelled
  exception) so finalize always runs; (3) sidecars now run with `stderr=PIPE` buffered into a 50-line ring, surfaced only
  on a broken pipe / non-zero exit (was `DEVNULL`) -- our only window into WHY a sidecar died. +1 test
  (`test_feed_survives_a_dead_sidecar_pipe`). **136 tests; ruff check + mypy --strict (66 files) green.** NB: the sidecar
  death itself is **not yet root-caused** (stderr was discarded at the time; the capture stays up, only that meeting's
  live Me/Them stopped, and the refine still recovers Them from them.wav). A sidecar restart/watchdog is a possible
  follow-up. **The 3a-tune VAD re-test was interrupted by this crash; the user then re-ran with the fix in place and
  confirmed "test is good" (tuned boundaries read well + no crash).**
- **2026-07-01 (docs sweep — post-pivot architecture docs; the pre-merge carry-over).** Rewrote `docs/architecture.md`
  + `docs/pipeline.md` end-to-end to the current Swift-sidecar architecture: the capture helper stays a lean PCM
  streamer; ASR + diarization run in `hearsay-{live,me,diarize,asr}` on the ANE (FluidAudio); Python feeds them + persists
  but runs no models. Removed every reference to the deleted `fusion/`, `MeetingDiarizer`, ONNX embedder, kaldi, online
  clusterer, whispercpp/mlx, and the torch/pyannote-in-Python path; documented the turn-driven live path, the Me-sidecar
  default + Silero fallback, and the offline refine (re-transcribe-per-turn + voiceprints). Also corrected
  `docs/development.md` (extras table → just `asr`+`bedrock`; config + troubleshooting tables) and `docs/api.md` (ASR
  picker → the single Parakeet model) — the older note claimed development.md was already fixed, but both still described
  the deleted backends/extras/settings. All four docs scan clean of the dead terms. Docs-only; no code touched
  (`git diff` = 4 files). Also ran `make swift-build` so all sidecars are built at `.build/debug/hearsay-{helper,diarize,
  asr,live,me}` (where the default `helper_path` looks) — the 3a on-device run is ready. **Still NEXT (needs the user):
  validate 3a (Me → `hearsay-me`) on a real meeting, then 3b (delete `vad/`/Silero + onnxruntime → zero Python ML deps),
  then merge.** NB left for merge: `CLAUDE.md` still lists `fusion/` + pywhispercpp/mlx. See
  [[hearsay-swift-pivot-direction]].
- **2026-07-01 (step 3a — Me → Swift `hearsay-me` sidecar, code-complete; needs on-device validation).** New Swift
  `hearsay-me` executable: local-mic PCM in → FluidAudio streaming VAD (`VadManager.processStreamingChunk`, 4096-sample
  frames) finds speech boundaries → Parakeet transcribes each utterance → emits `{text,start_s,end_s}` (Me is always the
  local speaker, no diarization). Python `LiveMeProcessor` owns it + persists "Me" segments. Factored the shared sidecar
  plumbing (spawn/feed/read/close) into `LiveSidecarProcessor`; `LiveThemProcessor` + `LiveMeProcessor` now subclass it
  (only `_handle` differs). Wired as the **default** Me path via `vad.me_sidecar` (default on); the Silero VAD + Python
  ASR path stays as the gated fallback (`HEARSAY_VAD__ME_SIDECAR=false`). Pipeline now builds a stream's Segmenter + the
  Python ASR backend **only for sidecar-less streams** (`_processor_for`), so the default path spawns no idle Parakeet/
  Silero. **135 tests (+ `LiveMeProcessor._handle`, + pipeline me_processor routing); ruff + mypy --strict (66 files) +
  swift build (5 products) + selftest + audit + licenses green.** Committed on `feat/fluidaudio-pivot`. **NEXT: restart
  serve + run a real meeting to validate the streaming VAD (the only unproven piece); then 3b — delete `vad/`/Silero +
  onnxruntime → zero ML deps. See [[hearsay-swift-pivot-direction]].**
- **2026-06-30 (eve, step 2 done — voiceprints from FluidAudio; online-clusterer fallback deleted).** `hearsay-diarize`
  now emits FluidAudio's per-speaker `speakerDatabase` (mean-of-segments embeddings) in its JSON; the Swift `Output`
  gained a `speakers:[{speaker,embedding}]` field. Python `OfflineDiarizer.diarize` returns a new `DiarizationResult`
  (`turns` + `embeddings`); the refine's `_recognize_speakers` stores each speaker's centroid (L2-normalized) + matches
  it against prior meetings — **no more audio-embedding pass**, so the ONNX embedder is gone. Deleting it orphaned the
  live online-clusterer fallback (it *needs* an embedder), so with the user's OK that whole fallback went too:
  **deleted** `diarization/{onnx_embedder,manager,features,base}.py`, `transcript/diarizer.py` (`MeetingDiarizer`), the
  `fusion/` package (`OnlineSpeakerClusterer`), `DiarizationBackendKind`, the `diarization.{enabled,live_streaming,
  backend,model,model_path,min_embed_ms,cluster_threshold}` settings, the `kaldi-native-fbank` dep + its mypy override,
  and the now-redundant `diarization` extra (== `asr`). `hearsay-live` is now the **only** live Them path (no off-switch;
  refine-at-finalize stays the safety net). `pipeline.py` lost its `diarizer`/`_resolve_speaker`/`bind_speaker`; `session.py`
  lost the embedder factory + the live-rename relay (the live sidecar has no bind API — a manual rename still relabels
  existing segments + is carried forward by the refine). Also **deleted the obsolete `scripts/transcribe_eval.py`** (a
  whisper.cpp A/B tool that imported the already-removed `whispercpp_backend` — it had silently broken `make typecheck`
  since the whisper drop). Tests: deleted `test_fusion.py` + `test_diarization.py`, dropped the 2 `MeetingDiarizer`
  pipeline tests, reworked the refine recognition test to source embeddings from the (stub) diarizer. README +
  development.md install commands fixed (`--extra asr`, no more `--extra diarization`). **`make ci` green: ruff + mypy
  --strict (64 files) + 133 pytest + swift build (4 products) + selftest + pip-audit + licenses; OpenAPI no drift.**
  Committed on `feat/fluidaudio-pivot`. **NEXT: step (3) Me → `hearsay-me` sidecar (needs on-device validation), then
  the architecture.md/pipeline.md docs sweep, then merge.** NB: step 2 invalidated stored centroids (different vector
  space) — old voiceprints re-seed on the next refine. See [[hearsay-swift-pivot-direction]].
- **2026-06-30 (eve, F2 validated live + step 4 done; steps 2-3 remain).** User confirmed F2 works on a real meeting
  ("seems fine"). Then did the fully-Swift **step 4 — dropped whisper.cpp**: deleted `WhisperCppBackend` + `pywhispercpp`
  (pruned from the venv + the `asr` extra), `build_asr` is Parakeet-only, `ASRBackendKind` reduced to `PARAKEET`, the
  model picker is now informational (one model). OpenAPI->TS regenerated; **web typechecks; 153 pass; ruff + mypy
  --strict (69 files) green.** Commit `3950ba1`. **Remaining fully-Swift steps (NEXT, documented for a clean
  continuation — see [[hearsay-swift-pivot-direction]]):** (2) **voiceprints from FluidAudio** — `TimedSpeakerSegment`
  carries an `embedding: [Float]`, so have `hearsay-diarize` emit per-speaker embeddings + the refine use them, then
  delete the ONNX embedder + kaldi (note: invalidates existing stored centroids — a different vector space). (3)
  **Me -> Swift** — a `hearsay-me` sidecar (FluidAudio streaming VAD + Parakeet → "Me" segments; the VAD streaming API
  is `makeStreamState`/`processStreamingChunk`), then drop Silero + the online clusterer/`MeetingDiarizer`/`vad/` (the
  whole VAD path) so the live pipeline is pure-sidecar. After (2)+(3) Python has zero ML deps (numpy stays only to pack
  PCM). Both need their own build + on-device validation; deferred this session for context.
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
