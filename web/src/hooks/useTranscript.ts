import { useEffect, useMemo, useReducer } from "react";

import { useSegments } from "../api/hooks";
import { getToken } from "../api/token";
import type { MeetingRead, SegmentRead } from "../api/types";
import { openTranscriptSocket } from "../api/ws";
import type { TranscriptEvent } from "../api/ws";

export interface TranscriptLine {
  kind: "partial" | "final";
  stream: TranscriptEvent["stream"];
  speaker_label: string;
  text: string;
  start_s: number;
  end_s: number;
}

interface State {
  finals: Map<string, TranscriptLine>;
  partials: Map<string, TranscriptLine>;
}

type Action =
  | { type: "reset" }
  | { type: "seed"; segments: SegmentRead[] }
  | { type: "event"; event: TranscriptEvent };

const lineKey = (line: { stream: string; start_s: number }): string =>
  `${line.stream}:${line.start_s}`;

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "reset":
      return { finals: new Map(), partials: new Map() };
    case "seed": {
      const finals = new Map(state.finals);
      for (const segment of action.segments) {
        finals.set(lineKey(segment), { ...segment, kind: "final" });
      }
      return { finals, partials: state.partials };
    }
    case "event": {
      const event = action.event;
      if (event.kind === "final") {
        const finals = new Map(state.finals);
        finals.set(lineKey(event), event);
        // A final supersedes the stream's in-flight partial.
        const partials = new Map(state.partials);
        partials.delete(event.stream);
        return { finals, partials };
      }
      const partials = new Map(state.partials);
      partials.set(event.stream, event);
      return { finals: state.finals, partials };
    }
  }
}

function init(): State {
  return { finals: new Map(), partials: new Map() };
}

// Merges DB-persisted finals with the live WebSocket stream into a single,
// time-ordered list. Rendering rule: one partial per stream, replaced by its
// next final (keyed by stream + start_s).
export function useTranscript(meeting: MeetingRead | null): TranscriptLine[] {
  const isLive = meeting?.status === "recording";
  const meetingId = meeting?.id ?? null;
  const segments = useSegments(meetingId);
  const [state, dispatch] = useReducer(reducer, undefined, init);

  useEffect(() => {
    dispatch({ type: "reset" });
  }, [meetingId]);

  useEffect(() => {
    if (segments.data) {
      dispatch({ type: "seed", segments: segments.data.items });
    }
  }, [segments.data]);

  useEffect(() => {
    if (!isLive || meetingId === null) return;
    return openTranscriptSocket(meetingId, getToken(), (event) =>
      dispatch({ type: "event", event }),
    );
  }, [isLive, meetingId]);

  return useMemo(() => {
    const lines = [...state.finals.values(), ...state.partials.values()];
    lines.sort((a, b) => a.start_s - b.start_s || a.stream.localeCompare(b.stream));
    return lines;
  }, [state]);
}
