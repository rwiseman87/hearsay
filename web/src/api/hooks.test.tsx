import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { describe, expect, it, type Mock, vi } from "vitest";

// Mock the one canonical fetch wrapper so the hooks run against scripted responses (no network).
vi.mock("./client", () => ({
  api: { get: vi.fn(), post: vi.fn(), put: vi.fn(), patch: vi.fn(), delete: vi.fn() },
}));

import { api } from "./client";
import { useSegments, useUpdateRecording } from "./hooks";
import { queryKeys } from "./queryKeys";
import type { RecordingSettings, SegmentRead, SettingsRead } from "./types";

const apiGet = api.get as unknown as Mock;
const apiPut = api.put as unknown as Mock;

function makeClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function wrapperFor(client: QueryClient) {
  return ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
}

function segRow(start: number): SegmentRead {
  return {
    id: `s${start}`,
    cluster_id: null,
    edited: false,
    end_s: start + 1,
    speaker_label: "Speaker 1",
    start_s: start,
    stream: "them",
    text: `line ${start}`,
  };
}

describe("useSegments pagination", () => {
  it("fetches every page so a >200-segment meeting is not silently truncated", async () => {
    apiGet
      .mockResolvedValueOnce({ total: 3, page: 1, page_size: 200, items: [segRow(1), segRow(2)] })
      .mockResolvedValueOnce({ total: 3, page: 2, page_size: 200, items: [segRow(3)] });

    const { result } = renderHook(() => useSegments("m1", false), { wrapper: wrapperFor(makeClient()) });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data).toHaveLength(3);
    expect(apiGet).toHaveBeenCalledTimes(2);
    expect(apiGet.mock.calls[0][0]).toContain("page=1");
    expect(apiGet.mock.calls[1][0]).toContain("page=2");
  });
});

describe("settings cache patch", () => {
  it("useUpdateRecording patches only the recording section into the settings cache", async () => {
    const client = makeClient();
    const initial = {
      recording: { record: false },
      speakers: { enabled: true },
      storage: {},
      models: {},
    } as unknown as SettingsRead;
    client.setQueryData(queryKeys.settings.all, initial);

    const updated = { record: true } as unknown as RecordingSettings;
    apiPut.mockResolvedValue(updated);

    const { result } = renderHook(() => useUpdateRecording(), { wrapper: wrapperFor(client) });
    act(() => {
      result.current.mutate(updated);
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const cached = client.getQueryData<SettingsRead>(queryKeys.settings.all);
    expect(cached?.recording).toEqual(updated);
    expect(cached?.speakers).toEqual(initial.speakers); // untouched — a patch, not a refetch
    expect(apiPut).toHaveBeenCalledWith("/api/settings/recording", updated);
  });
});
