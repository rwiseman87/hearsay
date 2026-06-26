# Architecture

This document explains the implementation that exists today (the Phase 0 capture spike, the
Phase 1 backend MVP, and the Phase 2 diarization layer): the process boundaries, why the
system is split the way it is, and what each Python package does. For the real-time data flow
see [pipeline.md](pipeline.md); for the HTTP/WebSocket surface see [api.md](api.md).

## Why three processes

Two hard constraints shaped the design:

1. **Capturing mic and system audio as separate channels gives "Me vs Them" for free.**
   The only remaining hard problem is naming the *multiple* remote speakers inside the
   system-audio stream (later phases).
2. **Real-time audio capture cannot be done reliably from Python.** PyObjC cannot safely
   run Core Audio realtime callbacks. So a thin **Swift helper** owns native capture and
   everything else lives in **Python**.

```mermaid
flowchart TB
  subgraph Helper["Swift helper (helper/)"]
    Tap["SystemAudioTap\nglobal-except-self process tap"]
    Mic["MicCapture\nAVAudioEngine"]
    Rs["Resampler -> 16 kHz mono\n+ one monotonic host_ts clock"]
    Tap --> Rs
    Mic --> Rs
  end
  subgraph Core["Python core (src/hearsay/)"]
    Sup["helper/ supervisor + channels"]
    Pipe["transcript/ pipeline\nVAD -> ASR (+ diarize Them)"]
    DB[("SQLite\nmeetings + segments\n+ clusters + identities")]
    MD["transcript.md\n+ meeting.json"]
    API["api/ FastAPI + WebSocket\n127.0.0.1 + token"]
    Sup --> Pipe --> DB
    Pipe --> MD
    Pipe --> API
  end
  Rs -- "media.sock (binary PCM)\ncontrol.sock (NDJSON)" --> Sup
  Core -- "spawns + supervises" --> Helper
```

The **core spawns and supervises the helper** (not the other way around), so capture
lifecycle and backpressure live in one place and survive helper restarts.

## Process boundary: the IPC contract

Two Unix-domain sockets in a per-session run directory, **owned (listened) by the core**
so they outlive helper restarts; the helper connects as a client.

- **`media.sock`** — binary framed PCM, helper -> core only. A fixed 28-byte little-endian
  header + payload. Both streams are multiplexed by a `stream` byte (0 = "me", 1 = "them").
  The load-bearing field is **`host_ts` (u64 ns, one monotonic clock)**: both streams are
  resampled to 16 kHz mono and stamped from the *same* clock at egress, so alignment is
  exact regardless of independent hardware clocks.
- **`control.sock`** — bidirectional NDJSON (one JSON object per line): core -> helper
  commands (with replies) and helper -> core events.

The frame layout and message schema are defined once in
[`shared/protocol/ipc.md`](../shared/protocol/ipc.md) and mirrored byte-for-byte by the
Swift `HearsayIPC.FrameCodec` and the Python `hearsay.helper.protocol`. Golden fixtures in
`shared/fixtures/frames.jsonl` pin the contract; both languages validate against them in
CI, so the two codecs cannot drift.

## The Python core, package by package

### `config/` — the single configuration source

`settings.py` is one `pydantic-settings` object loaded once and injected via DI. Every
tunable lives here: paths (`output_dir`, `models_dir`, `helper_path`),
the database URL, the server host/port, and nested `asr` / `vad` / `diarization` groups. A
model validator fills derived paths (e.g. the default SQLite URL and the Silero model path) so
the rest of the code never computes them ad hoc. Reads `HEARSAY_`-prefixed env vars (nested via
`__`, e.g. `HEARSAY_ASR__MODEL`, `HEARSAY_DIARIZATION__CLUSTER_THRESHOLD`).

### `enums.py`, `log.py` — shared primitives

`StrEnum`s used for DB columns and JSON (`Stream`, `MeetingStatus`, `SampleFormat`,
`ASRBackendKind`, ...). The binary IPC uses integer codes instead; that mapping is isolated
in `helper/protocol.py` so the wire format stays decoupled from these names. `log.py`
configures a stdlib JSON logger once and hands out named loggers via `get_logger`.

### `helper/` — the core side of the IPC

This is the client-of-the-contract, not the Swift helper. It turns the two raw sockets into
typed async streams:

- `protocol.py` — the `FrameCodec` mirror: encode/decode 28-byte media frames, plus
  `expected_payload_len()` (size the second read without decoding) and `audio_samples()`
  (int16/float32 payload -> float samples).
- `control.py` / `control_channel.py` — the NDJSON codec and an async read loop that
  correlates replies to in-flight commands by `id` (`call()`) and routes unsolicited events
  to a queue.
- `media_channel.py` — pumps frames into per-stream `asyncio.Queue`s of `AudioChunk`
  (samples + `host_ts`), counting `seq` gaps (dropped frames). A `None` item marks
  end-of-stream.
- `supervisor.py` — `HelperSupervisor` creates the run dir, listens on both sockets, spawns
  the helper, awaits its `hello`, and tears everything down cleanly.
- `capture_debug.py` — the Phase 0 truth-test harness behind `hearsay capture-debug`:
  drains both streams for N seconds and writes `me.wav` / `them.wav` with drift + skew
  diagnostics. This is how Me/Them separation was first proven on-device.

### `db/` + `models/` — persistence

Local-first **SQLite** via **async SQLAlchemy 2.0** (`sqlite+aiosqlite`), portable to
PostgreSQL by swapping the URL.

- `db/engine.py` — `create_engine()` sets SQLite pragmas on connect (`journal_mode=WAL`,
  `busy_timeout`, `foreign_keys=ON`) and explicit pool settings.
- `db/session.py` + `db/__init__.py` — the async sessionmaker and a small `Database` holder
  (engine + sessionmaker) that the app keeps in `app.state` and tests build against a temp file.
- `models/base.py` — the declarative `Base`: every table gets a UUID primary key and
  `created_at` / `updated_at`. `str_enum()` renders a `StrEnum` as a portable
  `VARCHAR` + `CHECK` storing the member *values*.
- `models/meeting.py`, `models/segment.py` — `Meeting` (title, folder, status, started/ended)
  and `Segment` (stream, speaker label, text, `start_s`/`end_s` meeting-relative seconds, nullable
  `cluster_id`), linked by a `meeting_id` FK with `ON DELETE CASCADE` and a `(meeting_id, start_s)`
  index.
- `models/cluster.py`, `models/identity.py` — `Cluster` (one diarized Them speaker per meeting:
  `ordinal` → "Speaker N", optional `identity_id`, a manual-lock flag, a `centroid` voiceprint
  BLOB, unique `(meeting_id, ordinal)`) and `Identity` (a cross-meeting person, unique
  `display_name`). Binding a cluster relabels its segments and the identity is suggested next time.
- `db/migrations/` — Alembic (async `env.py`, `render_as_batch` for SQLite; batch FK constraints
  are named). `alembic check` confirms the models match the latest revision.

### `schemas/` — the API boundary

Pydantic request/response models, kept separate from ORM models so the HTTP surface is
validated and decoupled from storage: `MeetingCreate`/`MeetingRead`, `SegmentRead` (carries the
resolved `speaker_label` + its `cluster_id`), the generic paginated `Page[T]`, the
`TranscriptEvent` (the WebSocket payload), the ASR picker schemas (`ASRStatus`, `ASRSelect`),
and the speaker schemas (`SpeakerRead`, `SpeakerRename`, `IdentityRead`).

### `services/` — business logic

`MeetingService` is the only place that reads/writes meetings + segments (routers call it and
stay thin): create, get, paginated list, add-segment, finalize, cascade delete, plus the
meeting-folder slug helper. Each method is a single logical write with one commit.
`SpeakerService` owns clusters + identities: create/list clusters (identity eager-loaded),
assign a segment to a cluster, `bind_cluster` (rename → get-or-create the identity, lock it, and
bulk-relabel that cluster's segments), and list identities for suggestions.

### `transcript/` — orchestration

The heart of a running meeting.

- `capture.py` — the `Capture` protocol (`start`/`stop`/`media`) and `HelperCapture`, which
  wraps `HelperSupervisor`: spawn the helper, send `start_capture`, expose the media channel,
  and tear down. The protocol is the seam that lets tests run the lifecycle without a helper.
- `pipeline.py` — `TranscriptionPipeline`: one consumer task per stream that segments audio
  (VAD), transcribes utterances off the event loop, diarizes finalized **Them** utterances, and
  fans results out to DB + `transcript.md` + WebSocket. See [pipeline.md](pipeline.md).
- `diarizer.py` — `MeetingDiarizer` (one per meeting): wraps the speaker embedder + the online
  clusterer + cluster-row persistence behind one async `resolve(utterance)` the pipeline calls
  for Them finals (embed off-loop → assign → label → `cluster_id`); `bind(ordinal, name)` relays
  a live rename. Me is never diarized.
- `broadcast.py` — `Broadcaster`, an in-process pub/sub that fans JSON event strings to
  active WebSocket subscribers.
- `session.py` — `MeetingSession` (one meeting: capture + pipeline + broadcaster) and
  `SessionManager` (owns the single active session; lock-guarded start/stop/delete +
  `relabel_speaker`, which binds in the DB and propagates the name to the live clusterer). The
  manager injects the ASR/VAD/sink/embedder factories, so the real ML stack is built on demand
  (the embedder factory degrades to `None` when no model is installed) and tests inject fakes.

### `vad/` — voice activity detection

- `base.py` — the `VAD` protocol (score a fixed-size frame) and `Segmenter`, the pure,
  heavily unit-tested state machine that turns frame scores into utterances using start/stop
  hysteresis and emits periodic partial snapshots.
- `silero.py` — the real `SileroVAD`, running the MIT Silero ONNX model directly on
  `onnxruntime` (no torch). Includes the pinned, integrity-checked model download.

### `asr/` — speech recognition

- `base.py` — the `ASRBackend` protocol (`transcribe(samples) -> [ASRSegment]`) and the
  `ASRSegment` type.
- `whispercpp_backend.py` — the default backend (pywhispercpp / whisper.cpp, Metal + CoreML,
  torch-free).
- `mlx_backend.py` — an opt-in Apple-MLX backend (behind the `accel` extra; pulls torch).
- `manager.py` — `build_asr()` selects the backend from settings; `resolve_model()` maps a
  friendly name (`large-v3-turbo`) to a backend-specific identifier (a GGML name vs an MLX
  Hugging Face repo); `available_models()` powers the picker.

### `diarization/` — speaker embeddings (Them voiceprints)

Torch-free by design, reusing the `onnxruntime` already vendored for VAD.

- `base.py` — the `SpeakerEmbedder` protocol (`embed(samples) -> L2-normalized vector`) plus
  `dim` / `model_id` (the model id tags stored centroids so a model change is detected).
- `features.py` — `compute_fbank()`: 80-dim Kaldi filterbank via `kaldi-native-fbank` (the same
  C++ extractor the models were trained with, so features are bit-identical — no torch) + the
  per-utterance mean normalization the models expect.
- `onnx_embedder.py` — `OnnxSpeakerEmbedder`: runs the embedding ONNX model on the CPU execution
  provider (fbank `[1, T, 80]` in, a `[1, dim]` voiceprint out).
- `manager.py` — the curated, license-vetted model registry (default **wespeaker CAM++_LM**,
  CC-BY-4.0, ungated, 512-d), pinned + sha256-checked downloads, and `build_embedder()`. pyannote
  stays an opt-in alternative behind the `diarization-pyannote` extra (pulls torch + a gated model).

### `fusion/` — speaker clustering

- `clustering.py` — `OnlineSpeakerClusterer`: **pure stdlib (no numpy)**, so it is dependency-free
  and exhaustively unit-tested. Each Them voiceprint is matched to the nearest speaker by cosine
  to a running centroid; at/above `threshold` it joins (updating the centroid), else it starts a
  new speaker. "Speaker N" ordinals follow first appearance; `bind()` manually names + locks a
  speaker; `add_seed()` recognizes a returning person from a prior meeting's voiceprint.

### `export/` — output sink

- `base.py` — the `TranscriptSink` protocol (`open`/`append`/`finalize`) plus the
  `MeetingMeta` / `TranscriptLine` value types. This seam is where a future central /
  object-store sink attaches without touching the pipeline.
- `local_markdown.py` — the only implementation today: a corruption-safe writer that appends
  complete, newline-terminated blocks live and atomically rewrites the transcript in
  timestamp order at finalize.

### `api/` — the loopback HTTP/WebSocket surface

- `app.py` — `create_app()` assembles the `AppContext`, the loopback-hardening middleware,
  and the routers.
- `security.py` / `deps.py` — Host + Origin allowlist, bearer-token checks, and Annotated DI
  dependencies (session, manager, token).
- `meetings.py`, `asr.py`, `speakers.py`, `ws.py` — the meetings CRUD router, the ASR model
  picker, the speakers router (list speakers + rename → identity + list identities), and the
  live-transcript WebSocket.

### `cli.py` — the `hearsay` command

`serve` (run the API), `live` (run the real pipeline and print transcripts — the on-device
validation harness), `fetch-models` (download the Silero VAD + speaker-embedding models), and
`capture-debug` (the Phase 0 raw-audio dump).

## Seams (build one, defer the rest)

Each protocol below has exactly one real implementation today plus injectable test doubles.
This is what keeps the MVP small while the plausible futures stay cheap.

| Seam | Protocol | Today | Deferred |
|---|---|---|---|
| Capture | `transcript.capture.Capture` | `HelperCapture` (Swift helper) | test fakes |
| VAD | `vad.base.VAD` | `SileroVAD` (onnxruntime) | whisper.cpp built-in VAD |
| ASR | `asr.base.ASRBackend` | `WhisperCppBackend` | `MlxBackend` (built, opt-in), SpeechAnalyzer |
| Embedder | `diarization.base.SpeakerEmbedder` | `OnnxSpeakerEmbedder` (onnxruntime) | pyannote (opt-in extra), voiceprint enrollment |
| Output | `export.base.TranscriptSink` | `LocalMarkdownSink` | central API / object store |

## The Swift helper (summary)

The helper is intentionally thin and stateless. `SystemAudioTap` builds a Core Audio process
tap configured **global-except-self** (dodges the Teams per-process-silent bug and covers
browser meeting apps) plus a private aggregate device, with a zero-buffer watchdog that
rebuilds both on sustained silence. `MicCapture` taps `AVAudioEngine` and rebuilds on
configuration changes. Both feed `Resampler` (16 kHz mono) and are stamped by `Clock`
(monotonic `host_ts`). `Serve.swift` drains the per-stream ring buffers to `media.sock` and
handles control commands. Details and the watchdog rationale are in the plan and
[`shared/protocol/ipc.md`](../shared/protocol/ipc.md).
