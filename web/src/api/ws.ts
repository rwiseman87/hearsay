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

// Opens the live-transcript socket for a meeting. Returns a disposer that closes
// it. The token rides as a query param (browsers cannot set WS headers).
export function openTranscriptSocket(
  meetingId: string,
  token: string,
  onEvent: (event: TranscriptEvent) => void,
): () => void {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  const url =
    `${scheme}://${window.location.host}/ws/meetings/${meetingId}` +
    `?token=${encodeURIComponent(token)}`;
  const socket = new WebSocket(url);
  socket.onmessage = (event) => {
    onEvent(JSON.parse(event.data as string) as TranscriptEvent);
  };
  return () => {
    socket.close();
  };
}
