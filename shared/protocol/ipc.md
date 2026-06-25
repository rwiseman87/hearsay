# hearsay IPC contract (v1)

Single source of truth for the helper <-> core boundary. The Swift `FrameCodec`
(`helper/Sources/HearsayIPC`) and the Python `hearsay.helper.protocol` module MUST
implement this byte-for-byte. Golden vectors in `shared/fixtures/` are validated by
both sides in CI to prevent drift.

## Topology

The **Python core** owns (listens on) two Unix domain sockets inside a per-session run
directory; the **Swift helper** connects to both as a client.

```
<run_dir>/media.sock     binary framed PCM        helper -> core   (uni-directional)
<run_dir>/control.sock   NDJSON commands/events   bi-directional
```

- The core creates `<run_dir>` (e.g. `~/Library/Application Support/hearsay/run/<session>/`),
  binds + listens on both sockets, then spawns `hearsay-helper serve --socket-dir <run_dir>`.
- The helper connects to `control.sock` first and sends a `hello` event, then connects to
  `media.sock` and sends a `hello` frame per stream before any audio.
- `stdout`/`stderr` of the helper are reserved for newline-delimited JSON logs only; never
  protocol data.

All multi-byte integers are **little-endian** (native on arm64). Time is a single monotonic
clock (`mach_absolute_time` -> nanoseconds) stamped by the helper at egress; the core treats
`host_ts` as opaque, monotonic, and shared across both streams (the alignment timeline).

## media.sock — framed PCM

Every message = a fixed **28-byte header** followed by `payload_len` bytes of payload.

| offset | size | field       | notes |
|-------:|-----:|-------------|-------|
| 0      | 1    | `magic`     | constant `0xA7` |
| 1      | 1    | `version`   | constant `1` |
| 2      | 1    | `type`      | `0`=audio `1`=hello `2`=heartbeat `3`=eos |
| 3      | 1    | `stream`    | `0`=mic ("me") `1`=system ("them") |
| 4      | 1    | `format`    | `0`=int16 `1`=float32 (PCM sample format) |
| 5      | 1    | `flags`     | reserved bitfield, `0` for now |
| 6      | 2    | `reserved0` | `0` |
| 8      | 4    | `seq`       | u32, per-`stream` monotonic from 0; gaps = dropped frames |
| 12     | 8    | `host_ts`   | u64 nanoseconds, monotonic, shared clock; timestamp of `payload[0]` |
| 20     | 4    | `n_samples` | u32, mono sample count in payload |
| 24     | 4    | `reserved1` | `0` |
| 28     | ...  | `payload`   | `n_samples * bytes_per_sample` bytes |

- `bytes_per_sample` = 2 for `int16`, 4 for `float32`. `payload_len` is derived, never sent.
- Audio is always **mono, 16000 Hz** (the helper resamples both sources). Sample rate is fixed
  by contract and not carried per-frame.
- **`type=audio`**: `n_samples > 0`, payload present. Cadence ~20-40 ms (320-640 samples).
- **`type=hello`**: first frame on each stream. `n_samples=0`, no payload; declares the stream
  is live in the given `format`. (Handshake metadata travels on `control.sock`; this is just a
  stream-open marker.)
- **`type=heartbeat`**: liveness when a stream is silent/idle. `n_samples=0`, no payload.
- **`type=eos`**: stream finished/stopping. `n_samples=0`, no payload.

Receivers MUST validate `magic` and `version` and drop/log-and-resync on mismatch.

## control.sock — NDJSON

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
| `check_permissions` | `{}` | `permission` map (see event) — probes TCC without capturing |
| `list_audio_processes` | `{}` | `{"processes": [{"pid", "bundle_id", "name"}]}` |
| `start_capture` | `{"tap_mode": "global_except_self"\|"meeting_app_only", "target": {"bundle_ids"?: [..], "window_id"?: int}, "sample_rate": 16000}` | `{"started": true}` |
| `stop_capture` | `{}` | `{"stopped": true}` |
| `set_active_speaker_mode` | `{"mode": "off"\|"ocr"\|"ax", "interval_ms"?: 1500}` | `{"mode": ".."}` |
| `set_target_window` | `{"window_id": int}` | `{"ok": true}` |
| `enable_ax` | `{"enabled": bool}` | `{"enabled": bool}` |
| `get_roster` | `{}` | `roster` data (see event) |
| `shutdown` | `{}` | `{"bye": true}` (helper then exits) |

### Events (helper -> core)

| `event` | `data` |
|---------|--------|
| `hello` | `{"helper_version": str, "protocol_version": 1, "pid": int}` (sent on connect) |
| `status` | `{"state": "capturing"\|"stopped"\|"degraded", "detail"?: str, "device"?: str}` |
| `permission` | `{"microphone", "audio_capture", "screen_recording", "accessibility", "calendar": "granted"\|"denied"\|"undetermined"}` |
| `tap_health` | `{"state": "ok"\|"zero_buffers"\|"recovered", "action"?: "rebuilt_tap"}` |
| `level` | `{"stream": "me"\|"them", "rms": float}` (meter; throttled) |
| `name_hint` | `{"source": "ocr"\|"ax", "text": str, "confidence": float, "bbox"?: [x,y,w,h]}` |
| `active_speaker` | `{"tile_bbox"?: [x,y,w,h], "changed": bool}` |
| `roster` | `{"event_id"?: str, "title"?: str, "start"?: str, "end"?: str, "attendees": [{"name", "email", "response"}]}` |
| `error` | `{"scope": str, "message": str, "fatal": bool}` |

All events carry `ts` (u64 ns, same clock as media `host_ts`) so name hints and active-speaker
changes align to audio/diarization turns in the fusion engine.

## Lifecycle

1. Core makes `<run_dir>`, listens on both sockets, spawns the helper with `--socket-dir`.
2. Helper connects to `control.sock` -> sends `hello` event; connects to `media.sock`.
3. Core sends `check_permissions`; renders onboarding from the `permission` reply/events.
4. Core sends `start_capture`; helper opens streams (each begins with a `hello` media frame),
   then streams `audio` frames + emits `status`/`tap_health`/`level`/hint events.
5. On helper crash/socket EOF, core keeps listening, marks session `degraded`, and respawns
   with backoff, replaying the last `start_capture`.
6. `shutdown` (or SIGTERM) stops capture and exits.

## Golden fixtures

`shared/fixtures/frames.jsonl` lists canonical frames as
`{"desc", "header": {field: value...}, "payload_hex": "..", "encoded_hex": ".."}`.
Both `FrameCodec` implementations MUST (a) decode `encoded_hex` to the given header+payload and
(b) re-encode the header+payload to exactly `encoded_hex`. CI fails on any mismatch.
