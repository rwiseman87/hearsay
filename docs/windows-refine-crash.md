# Windows refine crash + port handoff

Working notes for the open Windows work, written to be picked up cold. Everything below was
observed on the reference machine (Ryzen + RTX 5070 Ti + AMD integrated GPU, Windows 11) unless it
says otherwise. Claims are marked **proven** (observed directly) or **inferred** (reasoned from
evidence, not measured) — do not treat the inferred ones as settled.

## 1. The crash — FIXED

**Fixed** by bounding the embedder's input (see "The fix" below). The 370.8 s recording that crashed
every run now refines in 33.7 s / 33 segments. The rest of this section is kept because the failure
mode is a live hazard for any future onnx call, not because the bug is still open.

**Symptom.** Stop a meeting, press "Refine speakers", and the UI reports `failed to fetch`. Then
everything else breaks too — Show Files does nothing, the meeting list stops loading. That is one
failure, not many: `hearsay-core.exe` is dead, so every request fails. `hearsay-app.exe` stays up.

**Root cause chain (proven).**

1. During the refine's diarization pass, onnxruntime hits a shape error in the **TitaNet speaker
   embedding** model and **throws a C++ exception**:

   ```
   [E:onnxruntime:, sequential_executor.cc:572 onnxruntime::ExecuteKernel]
     Non-zero status code returned while running Where node.
     Name:'/encoder/encoder/encoder.0/mconv.3/Where_1'
     BroadcastIterator::Init axis == 1 || axis == largest was false.
     Attempting to broadcast an axis by a dimension other than 1. 12288 by 14794
   ```

   The `mconv` naming is NeMo's `ConvASREncoder`, i.e. `nemo_en_titanet_small.onnx` — the embedder,
   **not** pyannote segmentation.
2. sherpa-onnx's C API does not trap it, so the exception crosses the FFI boundary into Rust.
3. Rust cannot unwind a foreign exception and aborts the process:
   `fatal runtime error: Rust cannot catch foreign exceptions, aborting`.
4. The process dies with `0xC0000409` (`STATUS_STACK_BUFFER_OVERRUN`, which is what Rust's `abort()`
   raises via `__fastfail`). Windows Event Log shows the faulting module as `hearsay-core.exe`
   itself, because that abort happens in Rust runtime code compiled into the exe.

**This is not catchable.** No `catch_unwind`, no `Result`, no supervisor inside the process can
intercept step 3. It must be prevented at the input, or contained in another process.

### What triggers it

| input | result |
|---|---|
| `hot-ones-will-arnett/audio.wav`, 370.8 s stereo 16 kHz | **crash**, reproducible every run |
| same file truncated to 240 s | passes, 15 segments |
| synthetic TTS speech, 126 s mono | passes |
| synthetic TTS speech, 2 s mono | passes |

**The cap (proven, measured).** Both earlier inferences were right, and the cap is exact. Feeding
the embedder alone — no diarizer in the process — ascending slices of real audio
(`tests/embed_cap_probe.rs`, in its original probe form) gave:

| input | result |
|---|---|
| 10 / 30 / 60 / 90 / 110 / 118 / 120 / 121 / 122 / 122.5 / **122.8 s** | ok, dim 192 |
| **123.0 s** | crash — `12288 by 12298` |

TitaNet-small has a hard positional limit of **12288 encoder frames = 1_966_080 samples = 122.88 s**
at 160 samples/frame. 123.0 s is 12298 frames, over by 10.

**The crash site was our code, not sherpa's.** The probe above never calls `process()`, so the
offending tensor came from `SherpaDiarizer::diarize`, which concatenated **all** of a speaker's turns
into one buffer and embedded it in a single call for their voiceprint. sherpa's own internal
embedding is per-segment and stayed well under the cap — the full 370.8 s file diarizes fine.

Severity was as feared: any meeting where one person spoke more than ~123 s *in total* crashed the
app on refine. The 240 s cut passed because its dominant speaker fell under the cap.

### The fix (shipped)

`SherpaDiarizer::embed` now chunks at **30 s** (`MAX_EMBED_SAMPLES`, a wide margin under 122.88 s and
ample context for a speaker embedding) and averages the chunk vectors — each L2-normalized to
direction-only and weighted by its duration, so the result is the speaker's centroid rather than
whichever chunk was loudest. Only cosine similarity is ever applied to a voiceprint, so it is
deliberately left unnormalized.

Regression tests in `rust/crates/hearsay-inference/tests/embed_cap_probe.rs` (`--ignored`, they need
the ONNX models): one diarizes 300 s of tone — 2.4x the cap — and one checks the averaged voiceprint
still discriminates (same-source 1.000 vs cross-source 0.403), guarding the averaging from decaying
into mush. Neither can be a `should_panic`: the failure aborts the process, so the assertion is the
absence of death.

### Reproduce

`rust/crates/hearsay-inference/tests/refine_probe.rs` runs the exact Windows refine (sherpa
diarizer + whisper) outside the app, where the assert is visible. It now passes; before the fix it
aborted on any recording with a >123 s speaker:

```powershell
$env:LIBCLANG_PATH="C:\Program Files\LLVM\bin"
$env:VULKAN_SDK="C:\VulkanSDK\1.4.350.0"   # the build script panics without it
$env:CARGO_TARGET_DIR="$env:USERPROFILE\.hs"
$env:HEARSAY_BENCH_WAV="$env:APPDATA\com.hearsay.app\recordings\<meeting>\audio.wav"
cargo test --release --manifest-path rust\Cargo.toml -p hearsay-inference `
  --features sherpa,vulkan --test refine_probe -- --ignored --nocapture
```

Exit code `-1073740791` is the abort. The recording used for the repro is a YouTube video, not
private; the owner confirmed it is fine to use.

Generating test audio without touching real recordings (Windows TTS, 16 kHz mono):

```powershell
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000,
    [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
    [System.Speech.AudioFormat.AudioChannel]::Mono)
$s.SetOutputToWaveFile("$env:TEMP\synth.wav", $fmt)
1..6 | ForEach-Object { $s.Speak("...a paragraph of speech...") }
$s.Dispose()
```

### Ruled out (do not re-investigate)

Each was tested directly and passed:

- **Vulkan whisper** — 126 s of audio at 79x realtime, no crash.
- **The whole refine path on synthetic audio** — diarizer + whisper + Vulkan, 15 s, correct text.
- **AEC** — the full pipeline lifecycle (`hearsay-backends/tests/streaming_pipeline.rs`) with
  `--features sherpa,aec`, start through stop, passes.
- **A sherpa config knob** — `OfflineSpeakerDiarizationConfig` exposes only `min_duration_on`,
  `min_duration_off` and the clustering config. There is no max-segment/max-length setting.

### Why macOS is immune

`docs/architecture.md` on the sidecars: *"Each model owns its address space; a crash is contained."*
macOS runs diarization in the **`hearsay-diarize` process**. The Windows backend runs the same class
of model **in-process**, so an onnx throw takes down recording, refine and the HTTP API together.
The port dropped an isolation property the architecture calls for.

### Remaining hardening (not blocking)

The earlier plan here proposed windowed diarization stitched by cross-meeting voiceprint matching.
That is **not needed** — it assumed the long tensor was reaching sherpa's `process()`, which the
probe disproved. `process()` handles the full 370.8 s file; only our own concatenated embed
overflowed, so chunking that is the whole fix.

Still worth doing, in priority order:

1. **Run the Windows diarizer out-of-process**, mirroring macOS. Still a real architectural gap (see
   "Why macOS is immune" above), but now hardening against *future* onnx throws rather than a known
   crash. Any uncaught onnx exception anywhere in the in-process backend still takes down recording,
   refine and the HTTP API together.
2. **Report the TitaNet shape bug upstream** to k2-fsa/sherpa-onnx with the shapes above. sherpa's C
   API not trapping C++ exceptions is the more general defect: it makes any sherpa call a potential
   process kill for a Rust caller, since Rust cannot catch a foreign exception.

## 2. Build environment (Windows)

Prerequisites beyond the obvious (all now enforced up front by `scripts\build-windows.ps1`, which
throws with the remedy rather than failing deep in a build):

- **LLVM/libclang** — `llama-cpp-sys-2`'s bindgen needs it for the always-on `notes` feature.
- **Vulkan SDK** — the build enables `vulkan` by default.
- **Windows long paths** — `LongPathsEnabled=1` (elevated, see the script's message). ggml builds its
  Vulkan shader generator as a nested cmake sub-project whose paths exceed `MAX_PATH`.
- **Ninja** — under the Visual Studio generator that sub-build fails three separate ways (`MSB3491`
  .tlog over MAX_PATH, `FTK1011` from the native FileTracker which ignores the long-path setting,
  and `MSB8066`/`VCEnd`). Ninja produces none of them.
- **A short `CARGO_TARGET_DIR`** (≤24 chars, e.g. `%USERPROFILE%\.hs`) — `cl.exe` is not long-path
  aware and the shader sub-build sits ~230 characters below the target dir. Scoped to the core
  build only; the Tauri build must keep writing to `web\src-tauri\target`.

### Two traps worth knowing

- **`C:\Windows\System32\onnxruntime.dll` exists** (Windows ships its own, older, 1.17.1). Windows
  resolves a DLL from the loading binary's directory and *then* System32 — **ahead of PATH**. Since
  sherpa is linked as DLLs, any binary without an adjacent copy silently loads the system one and
  dies with `STATUS_ACCESS_VIOLATION`. `hearsay-inference/build.rs` copies the DLLs next to cargo's
  test binaries for this reason. Shipped binaries are fine (staged DLLs sit beside them).
- **sherpa must stay on its `shared` feature.** Its default `static` libs are built against the
  static CRT, which forces `-C target-feature=+crt-static` on everything; whisper.cpp pins CMP0091
  OLD, so its cmake appends `/MD` after cmake-rs's `/MT` and the link fails (`LNK2038`) with no
  toolchain-file fix, because platform defaults are set *after* a toolchain file runs. Linking the
  DLLs keeps the CRT inside them and removes the whole problem.

### Logs

The Windows shell is a GUI binary (no console), so the core's output is mirrored to:

```
%APPDATA%\com.hearsay.app\logs\core.log
```

Appended, so a crash survives the relaunch after it. A core exit is now surfaced in the UI whether
it happens during boot or mid-session.

## 3. State of the port

Committed on `feat/windows-port` (newest first):

| commit | what |
|---|---|
| _this_ | **refine crash fixed** — chunk the speaker embed under TitaNet's 12288-frame cap |
| `a80f5ca` | core log file + mid-session exit surfaced; `refine_probe`; unstick `streaming_pipeline` |
| `3a51011` | GUI subsystem (no stray terminal) |
| `e4b86d4` | case+punctuation restoration; aec on by default; DLL copy build script |
| `b33ad58` | scope `CARGO_TARGET_DIR` to the core build |
| `90310ed` | dead-mic watchdog + `capture_health` event + UI banner |
| `c179fe2` | 70M streaming zipformer (was 20M) |
| `4381561` | unblock the Windows build; Vulkan by default |

Measured numbers worth not re-deriving:

- Live ASR, 70M zipformer int8: **RTF 0.031** (20 ms per 560 ms chunk). The 20M it replaced dropped
  whole leading clauses. parakeet-unified 0.6B is **RTF 1.28** on CPU — not viable without a GPU
  execution path (onnxruntime has **no Vulkan provider**; DirectML or CUDA would be needed).
- Refine (whisper `small.en`, 7.4 s clip): **17.0 s** CPU → **5.33 s** discrete GPU → **3.27 s**
  integrated GPU. The iGPU wins on short clips because unified memory avoids the PCIe copy.
- `ggml-large-v3-turbo-q5_0` is **not** an upgrade here: 8.5x slower than `small.en` on CPU, tied on
  GPU, and it emitted no punctuation on the test clip. `small.en` stays.
- Punctuation restoration costs **~3.4 ms** per utterance (7.1 MB int8 model). It is a **no-op on
  uppercase input** — `Punctuator::restore` lowercases first and lets the model re-introduce case.

## 4. Other open items

- **Unverified: the punctuation model shared across threads.** `Punctuator` is handed to both stream
  workers and relies on the sherpa crate's blanket `unsafe impl Sync for OnlinePunctuation`. Whether
  sherpa's punctuation object is actually safe for concurrent calls has **not** been checked. If it
  is not, that is a data race. Worth confirming or giving each worker its own instance.
- **The original garbling report is still unconfirmed.** A muted/dead mic delivers exact-zero samples
  that WASAPI never flags as SILENT, and ASR hallucinates fluent text on digital silence rather than
  returning nothing. The dead-mic watchdog (`90310ed`) now surfaces that as a banner, but nobody has
  confirmed it was the cause. The outstanding check: does the Windows input level meter move when
  speaking into the mic?
- **Two pre-existing Windows test failures** (confirmed pre-existing by stashing and re-running on a
  clean tree, so they are not from this work):
  - `hearsay-core` `permissions_probe_degrades_when_helper_missing` — asserts
    `helper_available == false` from a nonexistent helper path, but Windows has no helper
    (`win_permissions.rs` ignores it and returns `available: true`). Inherently macOS-only.
  - `hearsay-orchestrator` `storage_override_pins_meeting_dir_off_the_default_root` — undiagnosed,
    possibly a real Windows path bug.
- **Installer waste.** The punctuation model directory is staged wholesale, so the bundle carries
  both `model.onnx` (28.3 MB) and `model.int8.onnx` (7.1 MB) though only int8 is loaded. The
  streaming model already stages int8-only; extend the same treatment.
- **Untested: CPU fallback with no Vulkan device.** ggml enumerates devices and should fall back, but
  the reference machine has two working GPUs so the empty-device path has never run. Accepted
  knowingly.
- **Machine state** (not repo state): `LongPathsEnabled` was set to 1 system-wide; LLVM, the Vulkan
  SDK and Ninja were installed; build caches live in `%USERPROFILE%\.hs` and `%USERPROFILE%\b`.
