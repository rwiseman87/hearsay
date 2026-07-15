import type { Stream } from "./types";

// Mirrors hearsay.schemas.segment.TranscriptEvent. The WebSocket is not part of
// the OpenAPI schema, so this type is hand-maintained to match the backend.
export interface TranscriptEvent {
  kind: "partial" | "final";
  stream: Stream;
  speaker_label: string;
  text: string;
  start_s: number;
  end_s: number;
}

// A warm-up status frame (not a transcript line): the transcription sidecars are still loading their
// models ("warming", sent as a snapshot on connect for a cold start) or have finished ("ready", the
// live transition). Lets the UI show a "preparing" notice instead of a silent gap. Also hand-kept.
export interface StatusEvent {
  kind: "status";
  state: "warming" | "ready";
}

// A backfill signal (not a transcript line): the server's broadcast buffer dropped events for a
// lagged subscriber, so the persisted transcript is ahead of this live stream. On receipt the client
// refetches persisted segments rather than diverging. Kept out of the reducer-facing `WsMessage`
// union — it never becomes a line. Hand-maintained like the others (the WebSocket is outside the
// OpenAPI codegen), so the server frame in routes/ws.rs must match this shape.
export interface ResyncEvent {
  kind: "resync";
}

// Anything the live socket can deliver, discriminated by `kind`.
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
