# API reference

The core serves a loopback REST + WebSocket API (axum) that the web UI consumes. Run it
with `make rust-serve`, which binds `127.0.0.1` on a free port and prints the URL with a
per-session token:

```
open: http://127.0.0.1:<port>/?token=<token>
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

### `PATCH /api/meetings/{id}` — rename / move a meeting

Updates the meeting's `title` and/or `folder_id`. Returns the updated `MeetingRead`; `404` if
unknown, `422` on a blank title.

### `PATCH /api/meetings/{id}/segments/{segment_id}` — edit a transcript line

Replaces one segment's `text` (the DB is the source of truth; `transcript.md` is re-exported
best-effort). Scoped to the meeting — `404` if the segment is not in it — `422` on empty/over-long
text, and `409` if the meeting is currently recording.

### `PUT /api/meetings/{id}/folder` — move a meeting into a folder

Body `{ "folder_id": "<uuid>" }` (or `null` to un-file to the root). `404` if the meeting or the
target folder is unknown.

### `POST /api/meetings/{id}/reveal` — reveal in the OS file manager

Opens the meeting's recordings folder (audio, transcript, notes) in Finder. The path is derived
server-side from the meeting, never client-supplied. `503` if it cannot be opened.

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

### `POST /api/meetings/{id}/rediarize` — re-run the offline refine

Re-diarizes and re-transcribes the Them track for the whole meeting (the "Refine speakers" button):
global clustering, overlap handling, and voiceprint recognition of returning people. Manual (locked)
labels are carried across. Returns the updated `MeetingRead`; `404` if unknown, `503` if the refine
model/sidecar is unavailable.

### `GET /api/identities` — known people (rename suggestions)

Paginated, most-recently-updated first. Powers the rename autocomplete — a name from one meeting
is offered in the next.

```json
// 200 OK
{ "total": 1, "page": 1, "page_size": 50, "items": [ { "id": "i7...", "display_name": "Alice", "email": null } ] }
```

## Folders

Meetings can be organized into a tree of folders (the sidebar). A folder has an optional `parent_id`;
deleting a folder cascade-deletes its sub-folders but **un-files** its meetings to the root (meetings
are never deleted). List endpoints are paginated.

- **`GET /api/folders`** — list folders (flat; the UI builds the tree).
- **`POST /api/folders`** — create — `{ "name", "parent_id"? }`; `422` on a blank name.
- **`PATCH /api/folders/{id}`** — rename — `{ "name" }`; `404` if unknown, `422` on blank.
- **`PUT /api/folders/{id}/parent`** — move — `{ "parent_id": "<uuid>" | null }`; rejects a cycle (a
  folder cannot become its own descendant).
- **`DELETE /api/folders/{id}`** — delete (cascades sub-folders, un-files meetings); `204`, `404` if unknown.

## Search

### `GET /api/search` — full-text transcript search

Query params: `q` (the search string), `page`, `page_size`. Runs an FTS5 match over finalized
**transcript** segments (not titles or notes). The query is sanitized to plain terms (FTS operators
stripped) and bound as a parameter, so an empty/all-punctuation query returns an empty page rather
than an error. Returns the paginated envelope of `SearchHit`s (meeting + matching segment + a
highlighted snippet).

```sh
curl "http://127.0.0.1:8137/api/search?q=budget&page=1&page_size=50" -H "Authorization: Bearer $TOKEN"
```

## Notes (optional local-LLM summary)

Available when the core is built with the `notes` feature; generation also requires a downloaded
notes model (see Models). Produces a summary + action items from the **finalized** transcript —
best-effort, never blocks stop.

- **`GET /api/meetings/{id}/notes`** — read the stored notes (`404` if none yet).
- **`POST /api/meetings/{id}/notes`** — generate / regenerate; `409` if the meeting is recording,
  `503` if no notes model is configured.
- **`PATCH /api/meetings/{id}/notes`** — edit the summary / action items; `422` on over-long input,
  `409` if recording.

## Models (notes-model download manager)

Present regardless of the `notes` build feature. A small fixed catalog of GGUF instruct models is
downloaded on demand into `HEARSAY_MODELS_DIR`, verified by SHA-256.

- **`GET /api/models/catalog`** — the catalog + each entry's installed state + the models directory.
- **`GET /api/models/download`** — the current download's status (idle / downloading + progress / error).
- **`POST /api/models/download`** — start downloading a catalog model — `{ "id" }` (one at a time).

## Settings

Editable preferences (a writable overlay over the env/startup defaults) plus read-only build and
permission facts.

### `GET /api/settings` — the effective settings

Returns every editable section — `recording`, `speakers`, `storage`, `models` — plus read-only
`storage_info` and `about`.

```json
// 200 OK
{
  "recording": { "record": true },
  "speakers": { "auto_refine": true, "recognition_threshold": 0.6 },
  "storage": { "output_dir": "/Users/you/.../outputs/recordings" },
  "models": { "notes_enabled": false, "notes_model": "", "notes_prompt": "<template with {transcript}>", "refine_model": ".../ggml-large-v3-turbo.bin" },
  "storage_info": { "output_dir": "...", "database_path": ".../hearsay.db", "tracked_bytes": 12345, "meeting_count": 3 },
  "about": { "app_version": "0.1.0", "environment": "production", "protocol_version": 1, "database_path": ".../hearsay.db" }
}
```

### `PUT /api/settings/{recording,speakers,storage,models}` — update one section

Each takes that section's body and returns it. `storage` validates `output_dir` (absolute, existing,
writable), `speakers` validates `recognition_threshold` in `0..=1`, and `models` validates the notes
model path (must exist and be a GGUF) and the prompt length — all `422` on a bad value.

### `POST /api/settings/reveal` — reveal the data folder

Opens the recordings root in the OS file manager (server-derived path). `503` if it cannot be opened.

### `GET /api/settings/permissions` — live TCC status

Briefly spawns the capture helper and reads its `check_permissions` snapshot + build version; never
persists. Degrades to `helper_available: false` + every field `unknown` when the helper is absent.

```json
// 200 OK
{ "helper_available": true, "helper_version": "0.1.0", "microphone": "granted",
  "audio_capture": "undetermined", "screen_recording": "undetermined",
  "accessibility": "undetermined", "calendar": "undetermined" }
```

## Status

### `GET /api/status` — live app status

A light poll for the header: the active meeting (if any), sidecar readiness/warming, and
refine/notes progress. Read-only and cheap to poll.

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
