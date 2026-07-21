# Windows refine crash + port handoff

Working notes for the open Windows work, written to be picked up cold. Everything below was
observed on the reference machine (Ryzen + RTX 5070 Ti + AMD integrated GPU, Windows 11) unless it
says otherwise. Claims are marked **proven** (observed directly) or **inferred** (reasoned from
evidence, not measured) — do not treat the inferred ones as settled.

## 1. The crash (open, highest priority)

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

**Decoding the shapes (inferred).** At 160 samples per frame: `12288` frames ≈ **122.9 s** and
`14794` frames ≈ **147.9 s**. So a fixed ~123 s cap is being overflowed by a ~148 s input.

**What that input is (inferred, and the key open question).** It is probably **all of one speaker's
audio concatenated** for their embedding, not a single turn: `min_duration_off` is 0.5 s, so any
half-second pause splits a turn, and a 148 s turn with no half-second pause is not plausible speech.

**If that inference is right, the severity is high**: any meeting where one person speaks more than
~123 s *in total* crashes the app on refine — most real meetings. The 240 s cut passed because its
dominant speaker fell under the cap.

**Verify this first** (cheap, and it decides the fix): diarize the 240 s cut, sum speech seconds per
speaker, and check the dominant speaker lands just under ~123 s. If instead it is per-turn, a much
smaller fix may do.

### Reproduce

`rust/crates/hearsay-inference/tests/refine_probe.rs` runs the exact Windows refine (sherpa
diarizer + whisper) outside the app, where the assert is visible:

```powershell
$env:LIBCLANG_PATH="C:\Program Files\LLVM\bin"
$env:HEARSAY_BENCH_WAV="$env:APPDATA\com.hearsay.app\recordings\<meeting>\audio.wav"
cargo test --release --manifest-path rust\Cargo.toml -p hearsay-inference `
  --features sherpa --test refine_probe -- --ignored --nocapture
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

### Proposed fix

Two parts; the second is the one that makes refine work.

1. **Contain it — run the Windows diarizer out-of-process**, mirroring macOS. A crash then fails one
   refine with an error instead of killing the app, and it covers every future onnx throw rather
   than just this input. Necessary but *not sufficient*: refine would still fail on most meetings.
2. **Bound the input — diarize in windows** (3–4 min) and stitch speakers across windows using the
   **cross-meeting voiceprint matching that already exists** in `hearsay-attribution`. This keeps
   per-speaker audio under the cap and reuses machinery the project already has. The stitching is
   the real design work: speaker identities from independent `process()` calls are arbitrary and
   must be merged by embedding similarity.

Also worth doing: report the TitaNet shape bug upstream to k2-fsa/sherpa-onnx with the shapes above.

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
