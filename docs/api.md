# API reference

The core serves a loopback REST and WebSocket API (axum) that the web UI consumes. Run it with
`make rust-serve`, which binds `127.0.0.1` on a free port and prints the URL with a per-session
token:

```
open: http://127.0.0.1:<port>/?token=<token>
```

The OpenAPI document is served at `/openapi.json`; it drives the TypeScript codegen.

When the web UI is built (`web/dist` present), the core also serves it: `GET /` returns
`index.html` with the session token injected as `window.__HEARSAY_TOKEN__` behind a per-response
CSP nonce, and hashed assets are served from `/assets`. Without the bundle, the core runs
API-only. `GET /` requires the `?token=` query parameter and the loopback Host/Origin checks; a
cross-site page cannot fetch it, so it cannot read the injected token.

## Authentication and loopback hardening

Loopback is not a security boundary: other local processes and browser pages can reach
`127.0.0.1`. Three checks gate every request:

| Check | Rule | On failure |
|---|---|---|
| Bearer token | REST: `Authorization: Bearer <token>`. The audio stream and the WebSocket accept `?token=<token>` instead, because those channels cannot set headers. | `401` |
| Host | The `Host` hostname must be `127.0.0.1`, `localhost`, or `::1` (blocks DNS rebinding). | `400` |
| Origin | If present (browser requests), the hostname must be loopback (blocks cross-site requests and WebSocket hijacking). | `403` |

The token is generated per `serve` process. Non-browser clients such as curl send no Origin and
pass the Origin check; the token is the real gate.

## Conventions

- REST base path is `/api`. Times are ISO 8601; segment `start_s`/`end_s` are meeting-relative
  seconds.
- List endpoints return a paginated envelope: `{ "total", "page", "page_size", "items" }`.
- Errors return `{ "detail": "..." }`.

## Meetings

### `POST /api/meetings` (start a meeting)

Spawns the helper, begins capture, and starts the transcription pipeline. One meeting may be
active at a time; a second start returns `409`.

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

### `GET /api/meetings` (list meetings)

Paginated, newest first. Query parameters: `page` (at least 1, default 1) and `page_size` (1 to
200, default 50).

```sh
curl "http://127.0.0.1:8137/api/meetings?page=1&page_size=50" -H "Authorization: Bearer $TOKEN"
```

```json
// 200 OK
{ "total": 1, "page": 1, "page_size": 50, "items": [ { "id": "0f7c...", "status": "finalized", "...": "..." } ] }
```

### `GET /api/meetings/{id}` (get one meeting)

Returns a `MeetingRead`, or `404` if unknown.

### `GET /api/meetings/{id}/segments` (list finalized segments)

Paginated, ordered by `start_s`. This is how a past meeting reloads from the database.

```json
// 200 OK
{
  "total": 2, "page": 1, "page_size": 200,
  "items": [
    { "id": "a1...", "stream": "me",   "speaker_label": "Me",        "cluster_id": null,   "text": "What is this about?",     "start_s": 8.0,  "end_s": 9.1 },
    { "id": "b2...", "stream": "them", "speaker_label": "Speaker 1", "cluster_id": "c9...", "text": "Your car is on its way.", "start_s": 28.4, "end_s": 30.0 }
  ]
}
```

### `POST /api/meetings/{id}/stop` (stop and finalize)

Stops capture, flushes the pipeline, rewrites `transcript.md` in timestamp order, and stamps
`ended_at`. Returns the updated `MeetingRead` immediately; when a refine or notes step will run,
the returned status is `refining` and flips to `finalized` when the background work completes.

### `POST /api/meetings/{id}/keep-recording` (dismiss the inactivity prompt)

Resets the active meeting's silence clock — the "Keep recording" action on the inactivity prompt, so
a present-but-quiet meeting is not nudged again or auto-ended. Returns `204`, or `404` if the
meeting is not the current recording session (there is no clock to reset).

### `DELETE /api/meetings/{id}` (delete a meeting)

Stops it if active, removes the database rows (segments, clusters, and notes cascade), and deletes
the on-disk folder. Returns `204`, or `404` if unknown.

### `GET /api/meetings/{id}/audio` (meeting audio, for playback)

Serves `audio.wav`: one timeline-accurate stereo track with Me on the left channel and Them on the
right. Sample N is meeting second N/16000, so a segment's `start_s` maps directly onto
`audio.currentTime`. Accepts the token as `?token=` (an `<audio>` element cannot set a header) or
as a bearer header, and supports `Range` requests (`206 Partial Content`) for seeking. `404` if the
meeting is unknown or was recorded with audio retention off.

### `PATCH /api/meetings/{id}` (rename or move)

Updates the meeting's `title` and/or `folder_id`. Returns the updated `MeetingRead`; `404` if
unknown, `422` on a blank title.

### `PATCH /api/meetings/{id}/segments/{segment_id}` (edit a transcript line)

Replaces one segment's `text`. The database is the source of truth; `transcript.md` is re-exported
best-effort. Scoped to the meeting (`404` if the segment is not in it), `422` on empty or
over-long text, `409` while the meeting is recording.

### `PUT /api/meetings/{id}/folder` (file into a folder)

Body `{ "folder_id": "<uuid>" }`, or `null` to un-file to the root. `404` if the meeting or the
target folder is unknown.

### `POST /api/meetings/{id}/reveal` (reveal in the file manager)

Opens the meeting's recordings folder (audio, transcript, notes) in Finder. The path is derived
server-side, never client-supplied. `503` if it cannot be opened.

## Speakers and identities

A diarized Them speaker is a cluster; renaming it binds the cluster to a cross-meeting identity.
Me is the microphone channel and is not a cluster.

### `GET /api/meetings/{id}/speakers` (list a meeting's speakers)

Paginated. Each item carries the cluster's resolved `label`: a bound name, else `Speaker N`.

```json
// 200 OK
{
  "total": 2, "page": 1, "page_size": 2,
  "items": [
    { "id": "c9...", "ordinal": 1, "label": "Alice",     "identity_id": "i7...", "locked": true  },
    { "id": "ca...", "ordinal": 2, "label": "Speaker 2", "identity_id": null,    "locked": false }
  ]
}
```

### `PUT /api/meetings/{id}/speakers/{cluster_id}` (rename a speaker)

Binds the cluster to an identity (get-or-create by name), locks it, and relabels that speaker's
segments. If the meeting is active, the name also propagates to the live pipeline so subsequent
utterances carry it. Returns the updated `SpeakerRead`; `404` if the cluster is not in the meeting,
`422` if the name is blank.

```sh
curl -X PUT http://127.0.0.1:8137/api/meetings/$MID/speakers/$CID \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"display_name": "Alice"}'
```

### `POST /api/meetings/{id}/rediarize` (re-run the offline refine)

Re-diarizes and re-transcribes the whole Them track (the "Refine speakers" button): global
clustering, overlap handling, and voiceprint recognition of returning people. Manual (locked)
labels are carried across. Returns the updated `MeetingRead`; `404` if unknown, `503` if the refine
model or sidecar is unavailable.

### `GET /api/identities` (known people)

Paginated, most recently updated first. Powers the rename autocomplete, so a name from one meeting
is suggested in the next.

```json
// 200 OK
{ "total": 1, "page": 1, "page_size": 50, "items": [ { "id": "i7...", "display_name": "Alice", "email": null } ] }
```

## Folders

Meetings can be organized into a tree of folders (the sidebar). A folder has an optional
`parent_id`. Deleting a folder cascade-deletes its sub-folders but un-files its meetings to the
root; meetings are never deleted by a folder operation. List endpoints are paginated.

- `GET /api/folders` lists folders flat; the UI builds the tree.
- `POST /api/folders` creates one from `{ "name", "parent_id"? }`; `422` on a blank name.
- `PATCH /api/folders/{id}` renames; `404` if unknown, `422` on blank.
- `PUT /api/folders/{id}/parent` moves; body `{ "parent_id": "<uuid>" | null }`. Rejects a cycle
  (a folder cannot become its own descendant).
- `DELETE /api/folders/{id}` deletes (cascades sub-folders, un-files meetings); `204`, or `404`.

## Search

### `GET /api/search` (full-text transcript search)

Query parameters: `q`, `page`, `page_size`. Runs an FTS5 match over finalized transcript segments
(not titles or notes). The query is sanitized to plain terms (FTS operators stripped) and bound as
a parameter, so an empty or all-punctuation query returns an empty page rather than an error.
Returns the paginated envelope of `SearchHit`s: meeting, matching segment, and a highlighted
snippet.

```sh
curl "http://127.0.0.1:8137/api/search?q=budget&page=1&page_size=50" -H "Authorization: Bearer $TOKEN"
```

## Notes (optional local-LLM summary)

Available when the core is built with the `notes` feature; generation also requires a downloaded
notes model (see Models). Produces a summary and action items from the finalized transcript.
Best-effort: a notes failure never blocks or fails a meeting.

- `GET /api/meetings/{id}/notes` reads the stored notes (`404` if none yet).
- `POST /api/meetings/{id}/notes` generates or regenerates; `409` while recording, `503` if no
  notes model is configured.
- `PATCH /api/meetings/{id}/notes` edits the summary or action items; `422` on over-long input,
  `409` while recording.

## Models (notes-model download manager)

Present regardless of the `notes` build feature, so the API surface is identical across builds. A
small fixed catalog of GGUF instruct models is downloaded on demand into `HEARSAY_MODELS_DIR` and
verified by SHA-256.

- `GET /api/models/catalog` returns the catalog, each entry's installed state, and the models
  directory.
- `GET /api/models/download` returns the current download's status: idle, downloading with
  progress, or error.
- `POST /api/models/download` starts downloading a catalog model, body `{ "id" }`; one at a time.

## Settings

Editable preferences (a writable overlay over the environment defaults) plus read-only build and
permission facts.

### `GET /api/settings` (the effective settings)

Returns every editable section (`recording`, `speakers`, `storage`, `models`) plus the read-only
`storage_info` and `about`.

```json
// 200 OK
{
  "recording": { "record": true, "inactivity_prompt_enabled": true, "inactivity_prompt_minutes": 5, "inactivity_end_minutes": 10 },
  "speakers": { "auto_refine": true, "recognition_threshold": 0.6 },
  "storage": { "output_dir": "/Users/you/.../outputs/recordings" },
  "models": { "notes_enabled": false, "notes_model": "", "notes_prompt": "<template with {transcript}>", "refine_model": ".../ggml-large-v3-turbo.bin" },
  "storage_info": { "output_dir": "...", "database_path": ".../hearsay.db", "tracked_bytes": 12345, "meeting_count": 3 },
  "about": { "app_version": "0.1.0", "environment": "production", "protocol_version": 1, "database_path": ".../hearsay.db" }
}
```

### `PUT /api/settings/{recording,speakers,storage,models}` (update one section)

Each takes that section's body and returns it. `storage` validates that `output_dir` is absolute,
existing, and writable; `speakers` validates `recognition_threshold` in `0..=1`; `models`
validates the notes model path (must exist and be a GGUF) and the prompt length; `recording`, when
the inactivity prompt is enabled, requires `inactivity_prompt_minutes` >= 1 and strictly less than
`inactivity_end_minutes` (<= 1440). All return `422` on a bad value.

### `DELETE /api/settings/models` (reset the models section)

Clears the stored `models` overrides so the section falls back to the environment defaults.
Returns the resulting effective section.

### `POST /api/settings/reveal` (reveal the data folder)

Opens the recordings root in the OS file manager (server-derived path). `503` if it cannot be
opened.

### `GET /api/settings/permissions` (live TCC status)

Briefly spawns the capture helper and reads its `check_permissions` snapshot and build version;
nothing is persisted. Degrades to `helper_available: false` with every field `unknown` when the
helper binary is absent.

```json
// 200 OK
{ "helper_available": true, "helper_version": "0.1.0", "microphone": "granted",
  "audio_capture": "undetermined", "screen_recording": "undetermined",
  "accessibility": "undetermined", "calendar": "undetermined" }
```

## Status

### `GET /api/status` (live app status)

A light poll for the header: the active meeting if any, sidecar readiness and warming, and refine
and notes progress. Read-only and cheap to poll.

## WebSocket: live transcript

```
ws://127.0.0.1:8137/ws/meetings/{id}?token=<token>
```

Connects to the active meeting's live stream. Auth happens at the HTTP handshake, before the
upgrade: a non-loopback Origin returns `403` and a bad token `401`. Connecting to a meeting that is
not the active session accepts and then closes cleanly (code 1000), so clients can tell that apart
from an auth failure.

While connected, the server pushes one JSON event per text frame. Transcript events:

```json
{ "kind": "partial", "stream": "them", "speaker_label": "Them",      "text": "your car is on its",      "start_s": 28.4, "end_s": 29.2 }
{ "kind": "final",   "stream": "them", "speaker_label": "Speaker 1", "text": "Your car is on its way.", "start_s": 28.4, "end_s": 30.0 }
```

`partial` events stream during ongoing speech and are not persisted; `final` events are also
written to the database and `transcript.md`. Clients render by `(stream, start_s)` and replace a
stream's partial with its next final. Me is always `"Me"`; Them partials carry the generic
`"Them"`, while finals carry the diarized label (`Speaker N` or a bound name). After a rename,
re-fetch `/speakers` and `/segments` to pick up new labels.

Three service events share the channel:

```json
{ "kind": "status", "state": "warming" }
{ "kind": "status", "state": "ready" }
{ "kind": "resync" }
{ "kind": "prompt", "silent_seconds": 300 }
```

- `status`: sent on connect when the transcription sidecars are still loading their models (a cold
  start), and again with `ready` once they finish, so the UI can show and clear a "preparing"
  notice.
- `resync`: the subscriber fell behind and the broadcast buffer dropped events. Finals are
  persisted before they are broadcast, so the database is a superset of the stream; on `resync`,
  re-fetch `GET /api/meetings/{id}/segments` and continue.
- `prompt`: no speech has been detected on either stream for `silent_seconds`, so the UI shows a
  "still recording?" banner. It is also sent on connect when a prompt is already active (a user
  reopening the window mid-silence). If the silence continues to the end threshold the meeting
  auto-ends with a logged transcript marker; the "Keep recording" action
  (`POST /api/meetings/{id}/keep-recording`) resets the clock. Thresholds are configured in the
  `recording` settings section.
