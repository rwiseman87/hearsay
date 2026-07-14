# API reference

The core serves a loopback REST + WebSocket API (axum) that the web UI consumes. Run it
with `make rust-serve`, which binds `127.0.0.1` on a free port and prints the URL with a
per-session token:

```
open: http://127.0.0.1:8137/?token=<token>
```

The OpenAPI document is served at `/openapi.json` (it drives the TypeScript codegen).

When the web UI is built (`web/dist` present), the core also serves it: `GET /` returns
`index.html` with the session token injected as `window.__HEARSAY_TOKEN__` (behind a
per-response CSP nonce), and hashed assets are served from `/assets`. If the bundle is not
built, the core runs API-only. `GET /` and `/assets` are gated by the Host/Origin checks below
but not the token (the page delivers the token); a cross-site Origin is rejected, so another
page cannot read it.

## Authentication and loopback hardening

Loopback is not a security boundary — other local processes and browser pages can reach
`127.0.0.1`. Three checks gate every request:

| Check | Rule | On failure |
|---|---|---|
| Bearer token | REST: `Authorization: Bearer <token>`. WebSocket: `?token=<token>` query param (browsers cannot set headers on a WS). | `401` |
| Host | `Host` header hostname must be `127.0.0.1`, `localhost`, or `::1` (blocks DNS rebinding). | `400` |
| Origin | If present (browser requests), its hostname must be loopback (blocks cross-site / CSWSH). | `403` |

The token is generated per `serve` process. Non-browser clients (curl) omit `Origin` and are
allowed through the Origin check; the token is the real gate.

## Conventions

- Base path for REST is `/api`. All times are ISO-8601; segment `start_s`/`end_s` are
  meeting-relative seconds.
- List endpoints return a paginated envelope: `{ "total", "page", "page_size", "items" }`.
- Errors return `{ "detail": "..." }`.

## Meetings

### `POST /api/meetings` — start a meeting

Spawns the helper, begins capture, and starts the transcription pipeline. One meeting may be
active at a time (`409` otherwise).

```sh
curl -X POST http://127.0.0.1:8137/api/meetings \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"title": "Weekly Sync"}'
```

```json
// 201 Created
{
  "id": "0f7c2c8e-2b1a-4a9c-9c0a-1f2e3d4b5a6c",
  "title": "Weekly Sync",
  "folder": "2026-06-26_1430_weekly-sync",
  "status": "recording",
  "started_at": "2026-06-26T14:30:00+00:00",
  "ended_at": null,
  "created_at": "2026-06-26T14:30:00+00:00",
  "updated_at": "2026-06-26T14:30:00+00:00"
}
```

`title` is optional; omitted, it defaults to a timestamp-derived name.

### `GET /api/meetings` — list meetings

Paginated, newest first. Query params: `page` (>= 1, default 1), `page_size` (1-200, default 50).

```sh
curl "http://127.0.0.1:8137/api/meetings?page=1&page_size=50" -H "Authorization: Bearer $TOKEN"
```

```json
// 200 OK
{ "total": 1, "page": 1, "page_size": 50, "items": [ { "id": "0f7c...", "status": "finalized", "...": "..." } ] }
```

### `GET /api/meetings/{id}` — get one meeting

Returns a `MeetingRead`, or `404` if unknown.

### `GET /api/meetings/{id}/segments` — list finalized segments

Paginated, ordered by `start_s`. This is how a past meeting reloads from the DB.

```json
// 200 OK
{
  "total": 2, "page": 1, "page_size": 200,
  "items": [
    { "id": "a1...", "stream": "me",   "speaker_label": "Me",       "cluster_id": null,    "text": "What is this about?", "start_s": 8.0, "end_s": 9.1 },
    { "id": "b2...", "stream": "them", "speaker_label": "Speaker 1", "cluster_id": "c9...", "text": "Your car is on its way.", "start_s": 28.4, "end_s": 30.0 }
  ]
}
```

### `POST /api/meetings/{id}/stop` — stop and finalize

Stops capture, flushes the pipeline, rewrites `transcript.md` in timestamp order, and sets
`status: "finalized"` + `ended_at`. Returns the updated `MeetingRead`.

### `DELETE /api/meetings/{id}` — delete a meeting

Stops it if active, removes the DB rows (segments cascade), and deletes the on-disk folder.
Returns `204`, or `404` if unknown.

### `GET /api/meetings/{id}/audio` — the meeting's audio (for playback)

Serves `audio.wav` (one timeline-accurate stereo track — Me on the left channel, Them on the
right; sample N is meeting second N/16000, so a segment's `start_s` maps straight onto
`audio.currentTime`). Auth accepts the token as a
`?token=` query param (an `<audio>` element cannot set an Authorization header) **or** a bearer
header. Supports `Range` requests (`206 Partial Content`) for seeking. `404` if the meeting is
unknown or was recorded with `audio.record` off.

## Speakers and identities

A diarized **Them** speaker is a *cluster*; renaming it binds the cluster to a cross-meeting
*identity*. **Me** is the mic channel and is not a cluster.

### `GET /api/meetings/{id}/speakers` — list a meeting's speakers

Paginated. Each item is the cluster's resolved `label` (a bound name, else `Speaker N`).

```json
// 200 OK
{
  "total": 2, "page": 1, "page_size": 2,
  "items": [
    { "id": "c9...", "ordinal": 1, "label": "Alice",     "identity_id": "i7...", "locked": true },
    { "id": "ca...", "ordinal": 2, "label": "Speaker 2", "identity_id": null,    "locked": false }
  ]
}
```

### `PUT /api/meetings/{id}/speakers/{cluster_id}` — rename a speaker

Binds the cluster to an identity (get-or-create by name), **locks** it, and relabels that
speaker's segments. If the meeting is active, the name also propagates to the live clusterer so
subsequent utterances carry it. Returns the updated `SpeakerRead`; `404` if the cluster is not in
the meeting, `422` if the name is blank.

```sh
curl -X PUT http://127.0.0.1:8137/api/meetings/$MID/speakers/$CID \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"display_name": "Alice"}'
```

### `GET /api/identities` — known people (rename suggestions)

Paginated, most-recently-updated first. Powers the rename autocomplete — a name from one meeting
is offered in the next.

```json
// 200 OK
{ "total": 1, "page": 1, "page_size": 50, "items": [ { "id": "i7...", "display_name": "Alice", "email": null } ] }
```

## Settings

Editable preferences (a writable overlay over the env/startup defaults) plus read-only build and
permission facts. See [settings-panels.md](settings-panels.md) for the panel model.

### `GET /api/settings` — the effective settings

Returns every section: `recording`, `speakers`, `storage`, plus read-only `storage_info` and `about`.

```json
// 200 OK
{
  "recording": { "record": true },
  "speakers": { "auto_refine": true, "recognition_threshold": 0.6 },
  "storage": { "output_dir": "/Users/you/.../outputs/recordings" },
  "storage_info": { "output_dir": "...", "database_path": ".../hearsay.db", "tracked_bytes": 12345, "meeting_count": 3 },
  "about": { "app_version": "0.1.0", "environment": "production", "protocol_version": 1, "database_path": ".../hearsay.db" }
}
```

### `PUT /api/settings/{recording,speakers,storage}` — update one section

Each takes that section's body and returns it. `storage` validates `output_dir` (absolute, existing,
writable) and `speakers` validates `recognition_threshold` in `0..=1` — both `422` on a bad value.

### `GET /api/settings/permissions` — live TCC status

Briefly spawns the capture helper and reads its `check_permissions` snapshot + build version; never
persists. Degrades to `helper_available: false` + every field `unknown` when the helper is absent.

```json
// 200 OK
{ "helper_available": true, "helper_version": "0.1.0", "microphone": "granted",
  "audio_capture": "undetermined", "screen_recording": "undetermined",
  "accessibility": "undetermined", "calendar": "undetermined" }
```

## WebSocket: live transcript

```
ws://127.0.0.1:8137/ws/meetings/{id}?token=<token>
```

Connects to the active meeting's live stream. The handshake is rejected (`close 1008`) on a
bad token or non-loopback Origin; connecting to a non-active meeting accepts then closes
(`1000`). While connected, the server pushes one JSON `TranscriptEvent` per text frame:

```json
{ "kind": "partial", "stream": "them", "speaker_label": "Them",      "text": "your car is on its", "start_s": 28.4, "end_s": 29.2 }
{ "kind": "final",   "stream": "them", "speaker_label": "Speaker 1", "text": "Your car is on its way.", "start_s": 28.4, "end_s": 30.0 }
```

`partial` events stream during ongoing speech and are not persisted; `final` events are also
written to the DB and `transcript.md`. Clients should render by `(stream, start_s)` and replace
a stream's partial with the next final. **Me** is always `"Me"`; **Them** partials are the generic
`"Them"`, while finals carry the diarized label (`Speaker N` or a bound name). After a rename,
re-fetch `…/speakers` and `…/segments` to pick up the new labels.
