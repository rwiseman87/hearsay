# hearsay IPC contract (v1)

Single source of truth for the helper/core boundary. The Swift `FrameCodec`
(`helper/Sources/HearsayIPC`) and the Rust `hearsay-ipc` codec MUST implement this byte-for-byte.
Golden vectors in `shared/fixtures/` are validated by both sides in CI to prevent drift.

## Topology

The Rust core owns (listens on) two Unix domain sockets inside a per-session run directory; the
Swift helper connects to both as a client.

```
<run_dir>/media.sock     binary framed PCM        helper -> core   (uni-directional)
<run_dir>/control.sock   NDJSON commands/events   bi-directional
```

- The core creates `<run_dir>` (for example
  `~/Library/Application Support/hearsay/run/<session>/`), binds and listens on both sockets, then
  spawns `hearsay-helper serve --socket-dir <run_dir>`.
- The helper connects to `control.sock` first and sends a `hello` event, then connects to
  `media.sock` and sends a `hello` frame per stream before any audio.
- The helper's `stdout`/`stderr` are reserved for newline-delimited JSON logs only, never protocol
  data.

All multi-byte integers are little-endian (native on arm64). Time is a single monotonic clock
(`mach_absolute_time` converted to nanoseconds) stamped by the helper at egress; the core treats
`host_ts` as opaque, monotonic, and shared across both streams (the alignment timeline).

## media.sock: framed PCM

Every message is a fixed 28-byte header followed by `payload_len` bytes of payload.

| offset | size | field       | notes |
|-------:|-----:|-------------|-------|
| 0      | 1    | `magic`     | constant `0xA7` |
| 1      | 1    | `version`   | constant `1` |
| 2      | 1    | `type`      | `0`=audio `1`=hello `2`=heartbeat `3`=eos |
| 3      | 1    | `stream`    | `0`=mic ("me") `1`=system ("them") |
| 4      | 1    | `format`    | `0`=int16 `1`=float32 (PCM sample format) |
| 5      | 1    | `flags`     | reserved bitfield; `0` in v1 |
| 6      | 2    | `reserved0` | `0` |
| 8      | 4    | `seq`       | u32, per-`stream` monotonic from 0; gaps mean dropped frames |
| 12     | 8    | `host_ts`   | u64 nanoseconds, monotonic, shared clock; timestamp of `payload[0]` |
| 20     | 4    | `n_samples` | u32, mono sample count in payload |
| 24     | 4    | `reserved1` | `0` |
| 28     | ...  | `payload`   | `n_samples * bytes_per_sample` bytes |

- `bytes_per_sample` is 2 for `int16` and 4 for `float32`. `payload_len` is derived, never sent.
- Audio is always mono at 16000 Hz (the helper resamples both sources). The sample rate is fixed
  by contract and not carried per frame.
- **`type=audio`**: `n_samples > 0`, payload present. Cadence roughly 20 to 40 ms (320 to 640
  samples).
- **`type=hello`**: the first frame on each stream. `n_samples=0`, no payload; declares the stream
  live in the given `format`. Handshake metadata travels on `control.sock`; this is just a
  stream-open marker.
- **`type=heartbeat`**: liveness when a stream is silent or idle. `n_samples=0`, no payload.
- **`type=eos`**: stream finished or stopping. `n_samples=0`, no payload.

Receivers MUST validate `magic` and `version`, and drop, log, and resync on a mismatch.

## control.sock: NDJSON

One UTF-8 JSON object per line, terminated by `\n`. Three shapes:

```jsonc
// command  (core -> helper)
{"id": 7, "cmd": "start_capture", "args": { ... }}
// reply    (helper -> core, correlates by id)
{"id": 7, "ok": true,  "result": { ... }}
{"id": 7, "ok": false, "error": {"code": "no_permission", "message": "..."}}
// event    (helper -> core, unsolicited, no id)
{"event": "tap_health", "ts": 123456789, "data": { ... }}
```

### Commands (core -> helper)

| `cmd` | `args` | `result` |
|-------|--------|----------|
| `ping` | `{}` | `{"pong": true}` |
| `check_permissions` | `{}` | `permission` map (see events); probes TCC without capturing |
| `list_audio_processes` | `{}` | `{"processes": [{"pid", "bundle_id", "name"}]}` |
| `start_capture` | `{"tap_mode": "global_except_self"\|"meeting_app_only", "target": {"bundle_ids"?: [..], "window_id"?: int}, "sample_rate": 16000}` | `{"started": true}` |
| `stop_capture` | `{}` | `{"stopped": true}` |
| `set_active_speaker_mode` | `{"mode": "off"\|"ocr"\|"ax", "interval_ms"?: 1500}` | `{"mode": ".."}` |
| `set_target_window` | `{"window_id": int}` | `{"ok": true}` |
| `enable_ax` | `{"enabled": bool}` | `{"enabled": bool}` |
| `get_roster` | `{}` | `roster` data (see events) |
| `shutdown` | `{}` | `{"bye": true}` (the helper then exits) |

> **Implemented subset.** The helper implements `ping`, `check_permissions`,
> `start_capture`, `stop_capture`, and `shutdown`; the remaining rows (`list_audio_processes`,
> `set_active_speaker_mode`, `set_target_window`, `enable_ax`, `get_roster`) are planned additions
> and currently answer `{"ok": false, "error": {"code": "unsupported", ...}}`. `start_capture`
> supports only `tap_mode: "global_except_self"` at the contract-fixed `sample_rate: 16000`;
> `meeting_app_only` and any other `sample_rate` are rejected as `unsupported` rather than
> silently accepted. A per-app or per-window `target` payload is not yet validated: the helper
> currently ignores it and starts the global tap.

### Events (helper -> core)

| `event` | `data` |
|---------|--------|
| `hello` | `{"helper_version": str, "protocol_version": 1, "pid": int}` (sent on connect) |
| `status` | `{"state": "capturing"\|"stopped"\|"degraded", "detail"?: str, "device"?: str}` |
| `permission` | `{"microphone", "audio_capture", "screen_recording", "accessibility", "calendar": "granted"\|"denied"\|"undetermined"}` |
| `tap_health` | `{"state": "ok"\|"zero_buffers"\|"recovered", "action"?: "rebuilt_tap"}` |
| `mic_health` | `{"state": "ok"\|"degraded"\|"recovered", "action"?: "restarted_engine"}` (mic engine restarted after an audio-config change) |
| `level` | `{"stream": "me"\|"them", "rms": float}` (meter; throttled) |
| `name_hint` | `{"source": "ocr"\|"ax", "text": str, "confidence": float, "bbox"?: [x,y,w,h]}` |
| `active_speaker` | `{"tile_bbox"?: [x,y,w,h], "changed": bool}` |
| `roster` | `{"event_id"?: str, "title"?: str, "start"?: str, "end"?: str, "attendees": [{"name", "email", "response"}]}` |
| `error` | `{"scope": str, "message": str, "fatal": bool}` |

All events carry `ts` (u64 nanoseconds, the same clock as media `host_ts`) so name hints and
active-speaker changes align to audio and diarization turns in the fusion engine.

## Lifecycle

1. The core makes `<run_dir>`, listens on both sockets, and spawns the helper with
   `--socket-dir`.
2. The helper connects to `control.sock`, sends a `hello` event, and connects to `media.sock`.
3. The core sends `check_permissions` and renders onboarding from the `permission` reply and
   events.
4. The core sends `start_capture`; the helper opens the streams (each begins with a `hello` media
   frame), then streams `audio` frames and emits `status`, `tap_health`, `level`, and hint events.
5. On a helper crash or media-socket EOF, the core finalizes the active meeting so it is not left
   falsely recording (the closed broadcast channel signals live subscribers), and the media reader
   drops and resyncs to the frame magic on a malformed frame rather than tearing capture down.
   Marking the session `degraded` and automatically respawning the helper with backoff to replay
   the last `start_capture` is not yet implemented.
6. `shutdown` (or SIGTERM) stops capture and exits.

## Golden fixtures

Both fixture files are generated from the Rust codec
(`cargo run -p hearsay-ipc --bin gen_fixtures`, wired into `make codegen`) and validated by both
languages in CI: the Rust golden-fixture tests in `cargo test`, and the Swift
`hearsay-helper selftest`. Never hand-edit them; `make codegen-check` fails on drift.

- `shared/fixtures/frames.jsonl` pins the media framing. Each line is
  `{"desc", "header": {field: value...}, "payload_hex": "..", "encoded_hex": ".."}`. Both codecs
  MUST decode `encoded_hex` to the given header and payload, and re-encode the header and payload
  to exactly `encoded_hex`.
- `shared/fixtures/control.jsonl` pins the NDJSON control protocol. Each line is
  `{"desc", "kind", "encoded"}`, where `kind` is one of `command`, `reply_ok`, `reply_fail`, or
  `event` and `encoded` is the canonical serialized line. Both sides MUST parse and re-serialize
  each fixture to the identical byte sequence, which pins key ordering and string escaping across
  the two languages.
