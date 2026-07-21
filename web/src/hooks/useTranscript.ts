import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useReducer, useState } from "react";

import { useSegments } from "../api/hooks";
import { notifyStillRecordingIfAway } from "../api/notify";
import { queryKeys } from "../api/queryKeys";
import { getToken } from "../api/token";
import type { MeetingRead, SegmentRead } from "../api/types";
import { openTranscriptSocket } from "../api/ws";
import type { ConnectionStatus, TranscriptEvent, WsMessage } from "../api/ws";

export interface TranscriptLine {
  kind: "partial" | "final";
  stream: TranscriptEvent["stream"];
  speaker_label: string;
  text: string;
  start_s: number;
  end_s: number;
  // The persisted segment id + edited flag, present only on DB-seeded finals (live WS lines have no
  // row yet). Drive the inline edit affordance + the "edited" badge; undefined lines are not editable.
  id?: string;
  edited?: boolean;
}

interface State {
  finals: Map<string, TranscriptLine>;
  partials: Map<string, TranscriptLine>;
  // True while the transcription sidecars are still loading their models (a cold start), so the UI
  // shows a "preparing" notice instead of a silent gap. Driven by the WS warm-up status frames.
  preparing: boolean;
  // Set when the server nudges that no speech has been detected for a while (the "still recording?"
  // banner); carries how long it has been silent. Cleared when speech resumes (any transcript line),
  // on reset, or by the user acting on the banner. Driven by the WS `prompt` frames.
  inactivityPrompt: { silentSeconds: number } | null;
  // True while the mic is delivering digital silence (muted or dead). Distinct from
  // `inactivityPrompt`, which means "no speech detected" on a working mic: here the signal path
  // itself is dead, and the transcript that keeps appearing is ASR hallucinating on zeros. Driven by
  // the WS `capture_health` frames, and only the server clears it (a Them line proves nothing about
  // the mic).
  micSilent: boolean;
}

type Action =
  | { type: "reset" }
  | { type: "seed"; segments: SegmentRead[]; replace: boolean }
  | { type: "event"; event: WsMessage }
  | { type: "dismissPrompt" };

const lineKey = (line: { stream: string; start_s: number }): string =>
  `${line.stream}:${line.start_s}`;

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "reset":
      return {
        finals: new Map(),
        partials: new Map(),
        preparing: false,
        inactivityPrompt: null,
        micSilent: false,
      };
    case "seed": {
      // While recording, merge the DB snapshot with the live WS finals (a stale fetch may lag
      // behind the socket). Once finalized, the DB is authoritative -- and the auto-refine has
      // rewritten the Them segments with new start_s keys, so replace outright (and drop any
      // lingering partial) to avoid showing both the old live finals and the refined ones.
      const finals = action.replace ? new Map<string, TranscriptLine>() : new Map(state.finals);
      for (const segment of action.segments) {
        finals.set(lineKey(segment), { ...segment, kind: "final" });
      }
      return {
        finals,
        partials: action.replace ? new Map() : state.partials,
        preparing: state.preparing,
        inactivityPrompt: state.inactivityPrompt,
        micSilent: state.micSilent,
      };
    }
    case "event": {
      const message = action.event;
      // Warm-up status (not a transcript line): show the notice while "warming", clear on "ready".
      if (message.kind === "status") {
        return { ...state, preparing: message.state === "warming" };
      }
      // Inactivity nudge (not a transcript line): raise the "still recording?" banner.
      if (message.kind === "prompt") {
        return { ...state, inactivityPrompt: { silentSeconds: message.silent_seconds } };
      }
      // Capture health (not a transcript line): the mic is dead, or has come back.
      if (message.kind === "capture_health") {
        return { ...state, micSilent: message.state === "silent" };
      }
      const event = message;
      // A transcript arriving proves the sidecars are serving (clear the "preparing" notice) and is
      // speech, so it clears any active inactivity prompt.
      if (event.kind === "final") {
        const finals = new Map(state.finals);
        finals.set(lineKey(event), event);
        // A final supersedes the stream's in-flight partial.
        const partials = new Map(state.partials);
        partials.delete(event.stream);
        return {
          finals,
          partials,
          preparing: false,
          inactivityPrompt: null,
          micSilent: state.micSilent,
        };
      }
      const partials = new Map(state.partials);
      partials.set(event.stream, event);
      return {
        finals: state.finals,
        partials,
        preparing: false,
        inactivityPrompt: null,
        micSilent: state.micSilent,
      };
    }
    case "dismissPrompt":
      return { ...state, inactivityPrompt: null };
  }
}

function init(): State {
  return {
    finals: new Map(),
    partials: new Map(),
    preparing: false,
    inactivityPrompt: null,
    micSilent: false,
  };
}

export interface TranscriptState {
  lines: TranscriptLine[];
  // Live socket state while recording; `null` when the meeting is finalized (no socket).
  connection: ConnectionStatus | null;
  // True while the live transcription sidecars are still loading their models, so the UI can show a
  // "preparing" notice during the start-up gap of a cold start (a pre-warmed start never sets it).
  preparing: boolean;
  // Set while the server is nudging that no speech has been detected for a while (the "still
  // recording?" banner); carries the silent duration. `null` when there is no active nudge.
  inactivityPrompt: { silentSeconds: number } | null;
  // True while the live mic is delivering digital silence, so the UI can warn that nothing is being
  // heard. Any transcript still arriving on Me while this is set is ASR hallucinating on zeros.
  micSilent: boolean;
  // Locally dismiss the inactivity banner (the "Keep recording" / "Stop" actions hide it until the
  // next server nudge). Does not reset the server clock — the caller pairs it with the keep-recording
  // mutation for that.
  dismissInactivityPrompt: () => void;
}

// Merges DB-persisted finals with the live WebSocket stream into a single,
// time-ordered list. Rendering rule: one partial per stream, replaced by its
// next final (keyed by stream + start_s). Also surfaces the live socket's
// connection state so the UI can show reconnecting instead of a frozen view.
export function useTranscript(meeting: MeetingRead | null): TranscriptState {
  const isLive = meeting?.status === "recording";
  const meetingId = meeting?.id ?? null;
  const segments = useSegments(meetingId, isLive);
  const queryClient = useQueryClient();
  const [state, dispatch] = useReducer(reducer, undefined, init);
  const [connection, setConnection] = useState<ConnectionStatus | null>(null);

  useEffect(() => {
    dispatch({ type: "reset" });
  }, [meetingId]);

  useEffect(() => {
    if (segments.data) {
      dispatch({ type: "seed", segments: segments.data, replace: !isLive });
    }
  }, [segments.data, isLive]);

  useEffect(() => {
    if (!isLive || meetingId === null) {
      setConnection(null);
      return;
    }
    return openTranscriptSocket(
      meetingId,
      getToken(),
      (event) => {
        dispatch({ type: "event", event });
        // A silence prompt for a user who has switched away from the window: also fire a native OS
        // notification so the nudge reaches them (desktop-only, unfocused-only; a no-op otherwise).
        if (event.kind === "prompt") {
          notifyStillRecordingIfAway(Math.round(event.silent_seconds / 60));
        }
      },
      setConnection,
      // On a reconnect or a server resync signal, persisted state may be ahead of the stream;
      // refetch segments so the reducer merges any missed finals (seed replace=false while live).
      () => {
        void queryClient.invalidateQueries({
          queryKey: queryKeys.meetings.segments(meetingId),
        });
      },
    );
  }, [isLive, meetingId, queryClient]);

  const lines = useMemo(() => {
    const merged = [...state.finals.values(), ...state.partials.values()];
    merged.sort((a, b) => a.start_s - b.start_s || a.stream.localeCompare(b.stream));
    return merged;
  }, [state]);

  return {
    lines,
    connection,
    preparing: isLive && state.preparing,
    inactivityPrompt: isLive ? state.inactivityPrompt : null,
    micSilent: isLive && state.micSilent,
    dismissInactivityPrompt: () => dispatch({ type: "dismissPrompt" }),
  };
}
