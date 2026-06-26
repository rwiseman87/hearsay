# API reference

The core serves a loopback REST + WebSocket API (FastAPI) that the web UI consumes. Run it
with `uv run hearsay serve`, which binds `127.0.0.1` on a free port and prints the URL and a
per-session token:

```
hearsay core on http://127.0.0.1:8137
open: http://127.0.0.1:8137/?token=<token>
```

OpenAPI/Swagger is available at `/docs`.

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
    { "id": "a1...", "stream": "me",   "speaker_label": "Me",   "text": "What is this about?", "start_s": 8.0, "end_s": 9.1 },
    { "id": "b2...", "stream": "them", "speaker_label": "Them", "text": "Your car is on its way.", "start_s": 28.4, "end_s": 30.0 }
  ]
}
```

### `POST /api/meetings/{id}/stop` — stop and finalize

Stops capture, flushes the pipeline, rewrites `transcript.md` in timestamp order, and sets
`status: "finalized"` + `ended_at`. Returns the updated `MeetingRead`.

### `DELETE /api/meetings/{id}` — delete a meeting

Stops it if active, removes the DB rows (segments cascade), and deletes the on-disk folder.
Returns `204`, or `404` if unknown.

## ASR model picker

### `GET /api/asr/models` — current selection + available models

```json
// 200 OK
{
  "backend": "whispercpp",
  "model": "large-v3-turbo",
  "models": [
    { "name": "large-v3-turbo", "label": "Large v3 Turbo", "installed": false },
    { "name": "base", "label": "Base (fast)", "installed": false }
  ]
}
```

`models` lists curated models plus any local GGML files found in `models_dir`.

### `PUT /api/asr/model` — switch model (and optionally backend)

Updates process settings; takes effect for the **next** meeting (a running meeting keeps the
backend it started with).

```sh
curl -X PUT http://127.0.0.1:8137/api/asr/model \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"model": "base"}'
```

Body: `{ "model": "<name|path|repo>", "backend": "whispercpp" | "mlx" (optional) }`. Returns
the updated `ASRStatus`.

## WebSocket: live transcript

```
ws://127.0.0.1:8137/ws/meetings/{id}?token=<token>
```

Connects to the active meeting's live stream. The handshake is rejected (`close 1008`) on a
bad token or non-loopback Origin; connecting to a non-active meeting accepts then closes
(`1000`). While connected, the server pushes one JSON `TranscriptEvent` per text frame:

```json
{ "kind": "partial", "stream": "them", "speaker_label": "Them", "text": "your car is on its", "start_s": 28.4, "end_s": 29.2 }
{ "kind": "final",   "stream": "them", "speaker_label": "Them", "text": "Your car is on its way.", "start_s": 28.4, "end_s": 30.0 }
```

`partial` events stream during ongoing speech and are not persisted; `final` events are also
written to the DB and `transcript.md`. Clients should render by `(stream, start_s)` and
replace a stream's partial with the next final.
