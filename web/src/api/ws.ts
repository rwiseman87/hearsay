import type { components } from "./schema";

type Schemas = components["schemas"];

// Live-transcript WebSocket frames. These are modeled in Rust (utoipa) and registered in the OpenAPI
// components, so they codegen into `schema.ts` and no longer drift from the server: the orchestrator
// pipeline that writes the wire bytes and these client types resolve to one source of truth. The
// socket itself is not an OpenAPI operation, so only the frame shapes are shared, not an endpoint.

// A transcript line: `partial` (interim; Them is streamed speaker-less) or `final` (persisted).
// `stream` narrows to "me" | "them".
export type TranscriptEvent = Schemas["TranscriptEvent"];

// A warm-up status frame (not a transcript line): "warming" while the transcription sidecars load
// their models (a snapshot on connect for a cold start), "ready" once they serve. Lets the UI show a
// "preparing" notice instead of a silent gap.
export type StatusEvent = Schemas["StatusEvent"];

// A backfill signal (not a transcript line): the server's broadcast buffer dropped events for a
// lagged subscriber, so the persisted transcript is ahead of this live stream. On receipt the client
// refetches persisted segments rather than diverging. Kept out of the reducer-facing `WsMessage`
// union — it never becomes a line.
export type ResyncEvent = Schemas["ResyncEvent"];

// Anything the live socket delivers on the transcript path, discriminated by `kind`.
export type WsMessage = TranscriptEvent | StatusEvent;

// Live-transcript connection state, surfaced to the UI so a dropped socket is visible instead of a
// silently frozen transcript.
export type ConnectionStatus = "connecting" | "open" | "reconnecting";

const MAX_BACKOFF_MS = 15_000;

// Opens the live-transcript socket for a meeting and keeps it open: on an unexpected close it
// reconnects with exponential backoff (the meeting is still recording, so the socket must recover
// without a page reload). Returns a disposer that stops reconnecting and closes the socket. The
// token rides as a query param (browsers cannot set WS headers).
export function openTranscriptSocket(
  meetingId: string,
  token: string,
  onEvent: (message: WsMessage) => void,
  onStatus?: (status: ConnectionStatus) => void,
  onResync?: () => void,
): () => void {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  const url =
    `${scheme}://${window.location.host}/ws/meetings/${meetingId}` +
    `?token=${encodeURIComponent(token)}`;

  let disposed = false;
  let socket: WebSocket | null = null;
  let attempt = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | undefined;

  const connect = () => {
    onStatus?.(attempt === 0 ? "connecting" : "reconnecting");
    socket = new WebSocket(url);
    socket.onopen = () => {
      // A reconnect (not the first connect) means the socket was down while the meeting kept
      // recording, so finals may have been persisted and missed on this stream; backfill on
      // recovery. Read `attempt` before it is reset.
      const reconnected = attempt > 0;
      attempt = 0;
      onStatus?.("open");
      if (reconnected) onResync?.();
    };
    socket.onmessage = (event) => {
      let parsed: WsMessage | ResyncEvent;
      try {
        parsed = JSON.parse(event.data as string) as WsMessage | ResyncEvent;
      } catch {
        // A malformed frame must not throw out of onmessage (which would kill the handler); drop it.
        return;
      }
      if (parsed.kind === "resync") {
        // Persisted state is ahead of this stream (server dropped events on lag); backfill instead
        // of silently diverging. Not a transcript line, so it never reaches the reducer.
        onResync?.();
        return;
      }
      onEvent(parsed);
    };
    // An errored socket always transitions to CLOSED and fires onclose, so reconnect is driven from
    // there alone (scheduling in both would double up).
    socket.onclose = () => {
      if (!disposed) scheduleReconnect();
    };
  };

  const scheduleReconnect = () => {
    onStatus?.("reconnecting");
    const delay = Math.min(1_000 * 2 ** attempt, MAX_BACKOFF_MS);
    attempt += 1;
    reconnectTimer = setTimeout(connect, delay);
  };

  connect();
  return () => {
    disposed = true;
    clearTimeout(reconnectTimer);
    socket?.close();
  };
}
