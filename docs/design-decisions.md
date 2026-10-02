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
rules interact badly, and the result is fragile. A Rust core is a single self-contained binary
with no interpreter to install.

**Swift only where the OS requires it.** Core Audio process taps, `AVAudioEngine`, and CoreML/ANE
access have no usable Rust bindings, and FluidAudio is a Swift package. Those live in the capture
helper and the sidecars; everything else is Rust.

**Tauri rather than Electron.** Tauri uses the system webview — WKWebView — instead of
bundling Chromium. A browser runtime would be the largest thing in an installer
whose whole point is that it is small enough to host.

**Models downloaded on first run rather than bundled.** The macOS model set is about 1.3 GB (Silero VAD, diarizer,
LS-EEND, Parakeet Ultra, Parakeet unified streaming; plus an optional notes model), close enough to
what a GitHub release asset can hold (2 GB) that bundling would crowd out the distribution channel. Downloading them once at first launch trades the offline-install property for a ~50 MB
installer; after that first run the app is as offline as it ever was.

**SQLite via SQLx rather than PostgreSQL.** This is a single-user desktop app; running a database
daemon would be infrastructure with no user. Going through SQLx rather than raw `rusqlite` keeps a
later move to a server cheap without paying for it now.

## Model selection

Per stage. The `hearsay-orchestrator` trait seams (`Transcriber` for live ASR and diarization,
`Refiner` for the offline refine, `Summarizer` for notes) keep each engine interchangeable.

| Stage | Model |
|---|---|
| Live ASR | Parakeet on the ANE: unified streaming for Me and for Them partials, Ultra for Them batch finals |
| Live diarization | FluidAudio streaming diarizer (LS-EEND), on CPU |
| Offline diarization | pyannote community-1, CoreML |
| Speaker embeddings | wespeaker_v2, 256-d |
| Me VAD | Silero |
| Offline refine ASR | Parakeet Ultra, on the ANE (`hearsay-diarize --asr ultra`) |
| Notes | local GGUF instruct model via llama.cpp |

**Why Parakeet for the refine.** The refine diarizes and transcribes in one sidecar run, and each
transcribed word is attributed to the diarizer turn it overlaps most (ties go to the shorter turn),
so the speaker boundaries and the words come from one pass over one set of timestamps. Parakeet
Ultra also measured better than the alternatives on the AMI ES2004a meeting (WER / cpWER, lower is
better):

| ASR | Near-field | Far-field |
|---|---|---|
| whisper large-v3-turbo | 0.214 / 0.271 | 0.299 / 0.343 |
| Parakeet v3 | 0.170 / 0.230 | 0.255 / 0.313 |
| Parakeet Ultra | 0.163 / 0.225 | 0.238 / 0.301 |
| Parakeet Phonon-2 | 0.226 / 0.276 | n/a |

Phonon-2 requires macOS 15, above the app's 14.4 floor. The gated figures in `make wer-eval` are the
Ultra row (`shared/eval/baseline-asr.json`). Because Parakeet decodes the track without a text
prompt carried between windows, it has no repetition-loop or silent-stall failure mode to repair;
a coverage guard (audible versus transcribed seconds) still flags a truncated transcript. The guard
counts any loud audio as speech, so background music with no voice lowers coverage.

The live diarizer (LS-EEND, AMI variant) scores DER 0.103 near-field but 0.600 far-field, where it finds
one of four speakers; the offline pyannote pipeline scores 0.144 and 0.194. Live labels are best
effort and the offline refine corrects them. Input conditioning (high-pass, level normalization, AGC)
does not change the live result, because the model's feature extractor normalizes gain itself. Timeline
thresholds and padding only trade misses against confusion, the other LS-EEND variants and the Nemotron 3
and Sortformer engines do not beat pyannote on both conditions, and the streaming path matches the
offline path, so the eval figures describe live behavior.

## Streaming versus offline

**Live diarization is deliberately approximate; accuracy work targets the offline refine.** A
streaming diarizer runs online with limited context, so it over- and under-merges. A whole-track pass after the meeting
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
enter an unrecoverable error state, and the GPU stays free for the notes LLM.

**The notes LLM in its own process.** llama.cpp generation holds gigabytes resident and can crash or
stall, which must not cost a live recording. `hearsay-notes` is therefore a standalone binary the
core spawns over stdio, and the shared prompt-building and reply-parsing logic lives in the
dependency-free `hearsay-notes-prompt` crate so the sidecar stays free of the refine's dependencies.

**Each CoreML model in its own sidecar.** A model crash is contained to that process rather than
taking down capture, and the capture helper stays free of CoreML entirely — so a model problem can
never cost the recording.

**All TCC-guarded APIs in one lean helper.** The microphone and the Core Audio system-audio tap are
confined to a single process. That keeps the permission surface small and auditable, and keeps the
binary that holds those grants free of heavyweight ML dependencies.

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
then unlink the original. Playback still serves WAV: the core decodes only the FLAC frames a range
request covers, because WebKit seeks a FLAC by estimating byte offsets, which lands tens of seconds
off when long muted stretches compress to almost nothing. Nothing decoded is written to disk.

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
cannot work inside the App Sandbox. Notarization additionally needs a paid Apple Developer account,
so recipients clear the quarantine flag once instead.
