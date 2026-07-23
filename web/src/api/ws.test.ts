import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { openTranscriptSocket } from "./ws";

// A minimal WebSocket stand-in: openTranscriptSocket only constructs it, assigns onopen/onmessage/
// onclose, and calls close(). The emit* helpers let a test drive the socket lifecycle deterministically.
class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  readyState = 0;
  close = vi.fn(() => {
    this.readyState = 3;
  });

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  emitOpen() {
    this.readyState = 1;
    this.onopen?.();
  }
  emitMessage(data: string) {
    this.onmessage?.({ data });
  }
  emitClose() {
    this.readyState = 3;
    this.onclose?.();
  }
}

function latest(): FakeWebSocket {
  const ws = FakeWebSocket.instances.at(-1);
  if (!ws) throw new Error("no socket has been opened");
  return ws;
}

beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("openTranscriptSocket", () => {
  it("opens with the url-encoded token and reports connecting then open", () => {
    const onStatus = vi.fn();
    const dispose = openTranscriptSocket("m-1", "tok en/x", vi.fn(), onStatus);

    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(latest().url).toMatch(/^ws:\/\/[^/]+\/ws\/meetings\/m-1\?token=tok%20en%2Fx$/);
    expect(onStatus).toHaveBeenNthCalledWith(1, "connecting");

    latest().emitOpen();
    expect(onStatus).toHaveBeenLastCalledWith("open");

    dispose();
  });

  it("parses frames to onEvent and silently drops malformed JSON", () => {
    const onEvent = vi.fn();
    const dispose = openTranscriptSocket("m-1", "t", onEvent, vi.fn());
    latest().emitOpen();

    const frame = {
      kind: "final",
      stream: "them",
      text: "hello",
      start_s: 1,
      end_s: 2,
      speaker_label: "Speaker 1",
    };
    latest().emitMessage(JSON.stringify(frame));
    expect(onEvent).toHaveBeenCalledWith(frame);

    latest().emitMessage("{ not json");
    expect(onEvent).toHaveBeenCalledTimes(1); // malformed frame dropped, handler survives

    dispose();
  });

  it("routes a resync frame to onResync, never to onEvent", () => {
    const onEvent = vi.fn();
    const onResync = vi.fn();
    const dispose = openTranscriptSocket("m-1", "t", onEvent, vi.fn(), onResync);
    latest().emitOpen();

    latest().emitMessage(JSON.stringify({ kind: "resync" }));
    expect(onResync).toHaveBeenCalledTimes(1);
    expect(onEvent).not.toHaveBeenCalled();

    dispose();
  });

  it("reconnects with exponential backoff, resetting only after a successful open", () => {
    const onStatus = vi.fn();
    const onResync = vi.fn();
    const dispose = openTranscriptSocket("m-1", "t", vi.fn(), onStatus, onResync);

    latest().emitOpen();
    expect(onResync).not.toHaveBeenCalled(); // the first connect is not a reconnect

    // First drop -> 1000ms backoff.
    latest().emitClose();
    expect(onStatus).toHaveBeenLastCalledWith("reconnecting");
    vi.advanceTimersByTime(999);
    expect(FakeWebSocket.instances).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeWebSocket.instances).toHaveLength(2);

    // The reconnect attempt drops again before opening -> 2000ms backoff (escalates).
    latest().emitClose();
    vi.advanceTimersByTime(1999);
    expect(FakeWebSocket.instances).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(FakeWebSocket.instances).toHaveLength(3);

    // A successful open fires the resync (persisted state may be ahead) and resets the backoff.
    latest().emitOpen();
    expect(onResync).toHaveBeenCalledTimes(1);
    latest().emitClose();
    vi.advanceTimersByTime(1000);
    expect(FakeWebSocket.instances).toHaveLength(4);

    dispose();
  });

  it("clears a pending reconnect timer and closes the socket on dispose", () => {
    const dispose = openTranscriptSocket("m-1", "t", vi.fn(), vi.fn());
    latest().emitOpen();
    const socket = latest();

    socket.emitClose(); // schedules a reconnect in 1000ms
    dispose();
    expect(socket.close).toHaveBeenCalled();

    vi.advanceTimersByTime(60_000);
    expect(FakeWebSocket.instances).toHaveLength(1); // timer cleared, no reconnect after dispose
  });
});
