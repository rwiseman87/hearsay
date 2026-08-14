# Design decisions

Why Hearsay is built the way it is. Each entry states the choice, what it was chosen over, and the
constraint that decided it.

## Related documents

| Document | Scope |
|---|---|
| [architecture.md](architecture.md) | What the resulting system looks like: processes, crates, data model. |
| [pipeline.md](pipeline.md) | How a meeting flows through it, stage by stage. |
| [voiceprints.md](voiceprints.md) | The speaker-recognition subsystem in depth. |
| [echo-cancellation.md](echo-cancellation.md) | The AEC and text-dedup layers in depth. |

## Language and runtime

**A Rust core rather than a Python backend.** Shipping CPython inside a signed, notarized desktop
app is the hardest part of packaging one: the interpreter, the native wheels, and the code-signing
rules interact badly, and the result is fragile. A Rust core is a single self-contained binary per
OS with no interpreter to install, and roughly 90 percent of it is shared between macOS and Windows.

**Swift only where the OS requires it.** Core Audio process taps, `AVAudioEngine`, ScreenCaptureKit,
and CoreML/ANE access have no usable Rust bindings, and FluidAudio is a Swift package. Those live in
the capture helper and the sidecars; everything else is Rust.

**Tauri rather than Electron.** Tauri uses the system webview — WKWebView on macOS, WebView2 on
Windows — instead of bundling Chromium. The installer already carries roughly 2.6 GB of models, so
adding a browser runtime on top is a cost with no offsetting benefit.

**SQLite via SQLx rather than PostgreSQL.** This is a single-user desktop app; running a database
daemon would be infrastructure with no user. Going through SQLx rather than raw `rusqlite` keeps a
later move to a server cheap without paying for it now.

## Model selection

Per stage, per OS. The two platforms run different engines because the ANE is a macOS-only
accelerator, and the trait seams (`Transcriber`, `Diarizer`, `Refiner`) keep them interchangeable.

| Stage | macOS | Windows |
|---|---|---|
| Live ASR | Parakeet TDT 0.6b, on the ANE | streaming zipformer (sherpa-onnx) |
| Casing and punctuation | emitted by Parakeet | a second model, `sherpa-onnx-online-punct-en` |
| Live diarization | FluidAudio streaming diarizer (LS-EEND), on CPU | **none** — live text is speaker-less |
| Offline diarization | pyannote community-1, CoreML | pyannote segmentation 3.0 (sherpa-onnx) |
| Speaker embeddings | wespeaker_v2, 256-d | TitaNet-small, 192-d |
| Me VAD | Silero | — |
| Offline refine ASR | whisper `ggml-large-v3-turbo` | whisper `ggml-small.en` |
| Notes | local GGUF instruct model via llama.cpp | same |

**Why not whisper for the live path?** whisper decodes in 30-second windows, so it cannot emit the
growing partial transcripts a live caption view needs. Parakeet and the streaming zipformer are
streaming-native. whisper earns its place in the offline refine, where whole-file context is an
advantage rather than a latency problem.

**Why a separate punctuation model on Windows?** The streaming zipformer emits bare uppercase text
with no punctuation, so the sherpa tier needs a second pass to make the transcript readable.
Parakeet emits punctuated, cased text directly. That is a concrete quality gap between the tiers,
not just a speed difference.

**Why FluidAudio stays the macOS accuracy tier.** Diarization tuning on the sherpa path was
exhausted under the dependency license gate and still plateaus: on a known-two-speaker clip it
settles on about three speakers across every clustering threshold that does not over-cluster badly,
where FluidAudio returns a clean two. The residual over-split is cleaned up afterwards by a
consolidation pass over whole-speaker centroids, which separate far better than sherpa's per-window
ones — a Windows-side repair, not a reason to change macOS.

**Why pyannote segmentation 3.0 on the sherpa path.** It is the only segmentation model in the
sherpa-onnx zoo under a permissive license. The better-performing Rev "reverb" models are
Non-Production/non-commercial, which the project's MIT/BSD/Apache-only dependency policy rules out.
The license gate picked the model here, not the benchmark.

**Why TitaNet-small for embeddings on that path.** It beat CAM++ as the embedder in the same
evaluation. The two embedding spaces are not interchangeable with the macOS one: a 256-d wespeaker
vector and a 192-d TitaNet vector are never compared, and a length mismatch is skipped rather than
scored, so voiceprints never cross-match between platforms.

**Why greedy decoding on the refine.** Beam search is too slow on `large-v3` for a pass that already
runs over the whole meeting.

## Streaming versus offline

**Live diarization is deliberately approximate; accuracy work targets the offline refine.** A
streaming diarizer runs online with limited context, so it over- and under-merges — and on Windows
there is no live diarization at all, only speaker-less text. A whole-track pass after the meeting
clusters globally, handles overlap properly, and is cheap on the ANE, so every meeting can end with
better labels than it streamed with. This is why "Refine speakers" exists as a user-facing action,
and why live label quality is not treated as a defect.

**Voiceprints are an offline-only artifact.** The live sidecars emit `{speaker, text, start_s,
end_s}` and no embeddings, so nothing during recording can write a centroid. Recognition of a
returning person therefore happens at refine time, never live.

**Them is segmented by speaker turn, not by voice activity.** A VAD cuts on silence rather than on
speaker change, so a quick exchange between two people lands in a single utterance that one label
cannot describe. Making the diarizer's turns the unit of segmentation splits fast turn-taking
correctly. Me is the microphone and is never diarized, so it uses a VAD.

**The transcript is rewritten at stop.** Live appends arrive in completion order, which interleaves
the two streams — a long Them turn can finish after a later Me utterance. A live append cannot
reorder past writes, so the finalize step re-reads every segment from the database sorted by start
time and rewrites the file, which also bakes in any names resolved along the way.

## Where inference runs

**Parakeet on the ANE, the streaming diarizer on CPU.** Two models contending for the Neural Engine
interfere with each other, so the live diarizer is loaded CPU-only on purpose and leaves the ANE to
the ASR. Running the live models on the GPU instead is worse still: a contended Metal pipeline can
enter an unrecoverable error state, and the GPU is left free for the offline whisper refine.

**The notes LLM in its own process.** llama.cpp and whisper.cpp each vendor `ggml`, and co-linking
them slows the refine by roughly 5x. `hearsay-notes` is therefore a standalone binary the core
spawns over stdio, and the shared prompt-building and reply-parsing logic lives in the
dependency-free `hearsay-notes-prompt` crate so the sidecar never pulls in whisper.

**Each CoreML model in its own sidecar.** A model crash is contained to that process rather than
taking down capture, and the capture helper stays free of CoreML entirely — so a model problem can
never cost the recording.

**All TCC-guarded APIs in one lean helper.** Microphone, system-audio tap, screen recording,
accessibility, and calendar access are confined to a single process. That keeps the permission
surface small and auditable, and keeps the binary that holds those grants free of heavyweight ML
dependencies.

## Audio handling

**SpeexDSP for echo cancellation, not macOS Voice Processing I/O.** VPIO cannot reference another
application's audio, so it cannot cancel the Them playback out of the mic — verified, not assumed.
WebRTC's AEC3 is the plausible acoustic upgrade; VPIO is not an option at all.

**A text-level dedup backstop behind the AEC.** Acoustic cancellation leaves residue, and residue
that survives into the ASR becomes the remote party's words attributed to the local user. The
backstop drops those lines after transcription, where they are easy to identify.

**One stereo recording, not two files.** Me on the left channel and Them on the right, placed by
meeting time, so sample N is meeting second N/16000. One file serves both in-browser playback and
the refine, and a segment's start time maps directly onto a seek position.

**Lossless FLAC archival, verified before the original is deleted.** An uncompressed hour is about
230 MB and never shrinks. FLAC is about 3x smaller and bit-identical, so playback, the refine, and
re-diarization are unaffected. The destructive step is ordered so failure can only cost disk space:
encode to a temporary, decode it back and compare sample for sample, rename into place, and only
then unlink the original.

## Data, API, and distribution

**The database is the source of truth.** `transcript.md`, `meeting.json`, and `notes.md` are
one-way exports, rebuilt from rows and never read back. That makes edits, renames, and re-diarization
single-writer operations with no file/database reconciliation problem.

**Notes are stored verbatim.** The model's reply is saved and rendered as Markdown exactly as
produced, rather than parsed into summary and action-item fields. The prompt template is
user-editable, so it dictates the format — parsing would fight whatever the user asked for.

**Loopback plus a per-session token plus Host and Origin allowlists.** `127.0.0.1` is not a security
boundary: any local process, and any web page in the user's browser, can reach it. The port is
treated as untrusted.

**One contract per boundary, both drift-checked in CI.** Golden fixtures pin the helper/core IPC
frames byte-for-byte in both Rust and Swift; the OpenAPI document generates the frontend's
TypeScript types. A feature that reaches the frontend but not the shipping core fails CI rather than
failing at runtime.

**Ad-hoc signed and not sandboxed, therefore not on the App Store.** The system-audio process tap
and the Accessibility path cannot work inside the App Sandbox. Notarization additionally needs a
paid Apple Developer account, so recipients clear the quarantine flag once instead.

## Open questions

One decision remains open, gated on verification rather than preference: whether to unify macOS onto
the same whisper.cpp/ONNX stack as Windows for one engine and the simplest distribution, or to keep
FluidAudio as a macOS-specific accuracy tier. The trait seams keep both available, and the sherpa
modules compile only under the `sherpa` feature, so the macOS bundle carries no onnxruntime today.
