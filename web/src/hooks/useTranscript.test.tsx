import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { MeetingRead, SegmentRead } from "../api/types";
import type { WsFrame } from "../api/ws";

// Drive the reducer through the real hook: capture the socket's onEvent so a test can inject frames,
// and control what useSegments returns (the DB seed) without any network. openTranscriptSocket reports
// "open" immediately so `connection` is observable.
const h = vi.hoisted(() => ({
  socket: {
    onEvent: undefined as ((f: WsFrame) => void) | undefined,
    onResync: undefined as (() => void) | undefined,
    dispose: vi.fn(),
  },
  segments: { data: undefined as SegmentRead[] | undefined },
  notify: vi.fn(),
}));

vi.mock("../api/ws", () => ({
  openTranscriptSocket: (
    _id: string,
    _token: string,
    onEvent: (f: WsFrame) => void,
    onStatus?: (s: string) => void,
    onResync?: () => void,
  ) => {
    h.socket.onEvent = onEvent;
    h.socket.onResync = onResync;
    onStatus?.("open");
    return h.socket.dispose;
  },
}));
vi.mock("../api/hooks", () => ({ useSegments: () => h.segments }));
vi.mock("../api/token", () => ({ getToken: () => "test-token" }));
vi.mock("../api/notify", () => ({ notifyStillRecordingIfAway: h.notify }));

import { useTranscript } from "./useTranscript";

const liveMeeting: MeetingRead = {
  id: "m-1",
  title: "Sync",
  folder: "2026/sync",
  folder_id: null,
  status: "recording",
  refine_incomplete: false,
  created_at: "t",
  updated_at: "t",
  started_at: "t",
  ended_at: null,
};
const finalizedMeeting: MeetingRead = { ...liveMeeting, id: "m-2", status: "finalized", ended_at: "t" };

function seg(over: Partial<SegmentRead> & { stream: SegmentRead["stream"]; start_s: number }): SegmentRead {
  return {
    id: `seg-${over.stream}-${over.start_s}`,
    cluster_id: null,
    edited: false,
    end_s: over.start_s + 1,
    speaker_label: "Speaker 1",
    text: "seeded",
    ...over,
  };
}

function renderTranscript(meeting: MeetingRead | null) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return renderHook((m: MeetingRead | null) => useTranscript(m), {
    initialProps: meeting,
    wrapper: ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client }, children),
  });
}

beforeEach(() => {
  h.socket.onEvent = undefined;
  h.socket.onResync = undefined;
  h.segments = { data: undefined };
});

describe("useTranscript", () => {
  it("seeds a finalized meeting from the DB, sorted, with no live socket", () => {
    h.segments = {
      data: [
        seg({ stream: "them", start_s: 2, text: "world" }),
        seg({ stream: "me", start_s: 1, text: "hello" }),
      ],
    };

    const { result } = renderTranscript(finalizedMeeting);

    expect(result.current.lines.map((l) => l.text)).toEqual(["hello", "world"]);
    expect(result.current.lines.every((l) => l.kind === "final")).toBe(true);
    expect(result.current.connection).toBeNull();
    expect(h.socket.dispose).not.toHaveBeenCalled();
  });

  it("merges a live seed with WS frames and lets a final supersede its stream's partial", () => {
    h.segments = { data: [seg({ stream: "me", start_s: 1, text: "seeded me" })] };

    const { result } = renderTranscript(liveMeeting);
    expect(result.current.connection).toBe("open");

    const partial: WsFrame = {
      kind: "partial",
      stream: "them",
      start_s: 5,
      end_s: 6,
      speaker_label: "",
      text: "typ...",
    };
    act(() => h.socket.onEvent!(partial));
    expect(result.current.lines.find((l) => l.stream === "them")?.kind).toBe("partial");

    const final: WsFrame = { ...partial, kind: "final", speaker_label: "Speaker 1", text: "typed" };
    act(() => h.socket.onEvent!(final));

    const them = result.current.lines.filter((l) => l.stream === "them");
    expect(them).toHaveLength(1);
    expect(them[0]).toMatchObject({ kind: "final", text: "typed" });
    expect(result.current.lines.find((l) => l.stream === "me")?.text).toBe("seeded me");
  });

  it("inserts a live partial in time order between finals (O(n) merge, not a resort)", () => {
    // Two finals at t=0 and t=10; a partial at t=5 must land between them — guards the sorted-finals
    // + binary-search partial merge that replaced the full re-sort on every event.
    h.segments = {
      data: [
        seg({ stream: "them", start_s: 0, text: "first" }),
        seg({ stream: "them", start_s: 10, text: "third" }),
      ],
    };
    const { result } = renderTranscript(liveMeeting);

    const partial: WsFrame = {
      kind: "partial",
      stream: "them",
      start_s: 5,
      end_s: 6,
      speaker_label: "",
      text: "second",
    };
    act(() => h.socket.onEvent!(partial));

    expect(result.current.lines.map((l) => l.text)).toEqual(["first", "second", "third"]);
  });

  it("tracks preparing / inactivity / mic-silent / paused flags and clears them on speech", () => {
    const { result } = renderTranscript(liveMeeting);

    act(() => h.socket.onEvent!({ kind: "status", state: "warming" }));
    expect(result.current.preparing).toBe(true);

    act(() => h.socket.onEvent!({ kind: "prompt", silent_seconds: 120 }));
    expect(result.current.inactivityPrompt).toEqual({ silentSeconds: 120 });
    expect(h.notify).toHaveBeenCalledWith(2); // round(120 / 60) minutes

    act(() => h.socket.onEvent!({ kind: "capture_health", state: "silent", stream: "me" }));
    expect(result.current.micSilent).toBe(true);

    act(() => h.socket.onEvent!({ kind: "capture_state", state: "paused" }));
    expect(result.current.paused).toBe(true);

    act(() => result.current.dismissInactivityPrompt());
    expect(result.current.inactivityPrompt).toBeNull();

    // A transcript line proves the sidecars serve and is speech: clears preparing + any prompt.
    act(() => h.socket.onEvent!({ kind: "status", state: "warming" }));
    act(() =>
      h.socket.onEvent!({
        kind: "final",
        stream: "me",
        start_s: 1,
        end_s: 2,
        speaker_label: "Me",
        text: "hi",
      }),
    );
    expect(result.current.preparing).toBe(false);
    expect(result.current.inactivityPrompt).toBeNull();
  });

  it("routes level frames to the level store, never into the transcript", () => {
    const { result } = renderTranscript(liveMeeting);
    let notified = 0;
    const unsub = result.current.levels.subscribe(() => {
      notified += 1;
    });

    act(() => h.socket.onEvent!({ kind: "level", stream: "me", rms: 0.4 }));
    expect(result.current.levels.getSnapshot()).toBe(0.4);
    expect(notified).toBe(1);
    expect(result.current.lines).toHaveLength(0);

    act(() => h.socket.onEvent!({ kind: "level", stream: "them", rms: 0.7 }));
    expect(result.current.levels.getSnapshot()).toBe(0.7); // max(me, them)

    unsub();
  });

  it("resets the transcript when the meeting changes", () => {
    h.segments = { data: [seg({ stream: "me", start_s: 1, text: "m1 line" })] };
    const { result, rerender } = renderTranscript(liveMeeting);
    expect(result.current.lines).toHaveLength(1);

    h.segments = { data: undefined };
    act(() => rerender({ ...liveMeeting, id: "m-99" }));

    expect(result.current.lines).toHaveLength(0);
  });
});
