import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { beforeEach, describe, expect, it, type Mock, vi } from "vitest";

// Mock the one canonical fetch wrapper so the hooks run against scripted responses (no network).
vi.mock("./client", () => ({
  api: { get: vi.fn(), post: vi.fn(), put: vi.fn(), patch: vi.fn(), delete: vi.fn() },
}));

import { api } from "./client";
import {
  useDeleteVoiceprint,
  useForgetVoice,
  useMeeting,
  useMeetings,
  useMergeSpeakers,
  useReassignSpeaker,
  useRenameIdentity,
  useSegments,
  useUpdateRecording,
} from "./hooks";
import { queryKeys } from "./queryKeys";
import type { RecordingSettings, SegmentRead, SettingsRead } from "./types";

const apiGet = api.get as unknown as Mock;
const apiPut = api.put as unknown as Mock;
const apiPatch = api.patch as unknown as Mock;
const apiPost = api.post as unknown as Mock;
const apiDelete = api.delete as unknown as Mock;

// Each test reads its own calls off the wrapper mock, so no test may inherit another's.
beforeEach(() => {
  vi.clearAllMocks();
});

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

describe("useMeetings", () => {
  it("passes the folder, search and sort filters to the server", async () => {
    apiGet.mockResolvedValue({ total: 0, page: 2, page_size: 50, items: [] });

    const { result } = renderHook(
      () => useMeetings({ page: 2, folderId: "f-1", q: " kickoff ", sort: "oldest" }),
      { wrapper: wrapperFor(makeClient()) },
    );
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const url = new URL(apiGet.mock.calls[0][0], "http://localhost");
    expect(url.searchParams.get("page")).toBe("2");
    expect(url.searchParams.get("page_size")).toBe("50");
    expect(url.searchParams.get("folder_id")).toBe("f-1");
    expect(url.searchParams.get("q")).toBe("kickoff");
    expect(url.searchParams.get("sort")).toBe("oldest");
    expect(url.searchParams.get("unfiled")).toBeNull();
  });

  it("asks for the unfiled bucket by flag, not by a folder id", async () => {
    apiGet.mockResolvedValue({ total: 0, page: 1, page_size: 50, items: [] });

    const { result } = renderHook(() => useMeetings({ unfiled: true }), {
      wrapper: wrapperFor(makeClient()),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const url = new URL(apiGet.mock.calls[0][0], "http://localhost");
    expect(url.searchParams.get("unfiled")).toBe("true");
    expect(url.searchParams.get("folder_id")).toBeNull();
  });
});

describe("useMeeting", () => {
  it("fetches the open meeting by id rather than finding it in a listed page", async () => {
    apiGet.mockResolvedValue({ id: "m-99", title: "Deep in the library" });

    const { result } = renderHook(() => useMeeting("m-99"), { wrapper: wrapperFor(makeClient()) });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(apiGet.mock.calls[0][0]).toBe("/api/meetings/m-99");
  });

  it("stays idle with no selection", () => {
    const { result } = renderHook(() => useMeeting(null), { wrapper: wrapperFor(makeClient()) });
    expect(result.current.fetchStatus).toBe("idle");
    expect(apiGet).not.toHaveBeenCalled();
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

describe("useReassignSpeaker", () => {
  it("PATCHes the segment-speaker route and refreshes the transcript + speakers", async () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    apiPatch.mockResolvedValue(segRow(1));

    const { result } = renderHook(() => useReassignSpeaker("m1"), { wrapper: wrapperFor(client) });

    // Reassign to an existing cluster.
    act(() => {
      result.current.mutate({ segmentId: "s1", clusterId: "c9" });
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(apiPatch).toHaveBeenCalledWith("/api/meetings/m1/segments/s1/speaker", {
      cluster_id: "c9",
      display_name: undefined,
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.meetings.segments("m1") });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.meetings.speakers("m1") });

    // Assign to a person by name.
    act(() => {
      result.current.mutate({ segmentId: "s2", displayName: "Dana" });
    });
    await waitFor(() => expect(apiPatch).toHaveBeenCalledTimes(2));
    expect(apiPatch).toHaveBeenLastCalledWith("/api/meetings/m1/segments/s2/speaker", {
      cluster_id: undefined,
      display_name: "Dana",
    });
  });
});

describe("useMergeSpeakers", () => {
  it("POSTs the merge and refreshes the transcript, speakers and voiceprint roster", async () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    apiPost.mockResolvedValue({ total: 1, page: 1, page_size: 1, items: [] });

    const { result } = renderHook(() => useMergeSpeakers("m1"), { wrapper: wrapperFor(client) });
    act(() => {
      result.current.mutate({ clusterId: "c1", into: "c2" });
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(apiPost).toHaveBeenCalledWith("/api/meetings/m1/speakers/c1/merge", { into: "c2" });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.meetings.segments("m1") });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.meetings.speakers("m1") });
    // A merge deletes the source cluster, taking its stored voice sample with it.
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.voiceprints.all });
    // But it neither creates nor renames a person, so the identity list is still good.
    expect(invalidate).not.toHaveBeenCalledWith({ queryKey: queryKeys.identities.all });
  });
});

describe("voiceprint mutations", () => {
  it("useRenameIdentity invalidates every meeting, since the label changed in all of them", async () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    apiPatch.mockResolvedValue({ id: "i1", display_name: "Alicia", email: null });

    const { result } = renderHook(() => useRenameIdentity(), { wrapper: wrapperFor(client) });
    act(() => {
      result.current.mutate({ identityId: "i1", displayName: "Alicia" });
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(apiPatch).toHaveBeenCalledWith("/api/identities/i1", { display_name: "Alicia" });
    // The broad prefix on purpose: unlike a per-meeting speaker rename, this rewrote speaker_label
    // across every meeting the person appears in, so no cached meeting query is still correct.
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.meetings.all });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.voiceprints.all });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.identities.all });
  });

  it("forgetting a voice touches only the roster, since no label or transcript changes", async () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    apiDelete.mockResolvedValue(undefined);

    const sample = renderHook(() => useDeleteVoiceprint(), { wrapper: wrapperFor(client) });
    act(() => {
      sample.result.current.mutate("c1");
    });
    await waitFor(() => expect(sample.result.current.isSuccess).toBe(true));
    expect(apiDelete).toHaveBeenCalledWith("/api/voiceprints/c1");

    const person = renderHook(() => useForgetVoice(), { wrapper: wrapperFor(client) });
    act(() => {
      person.result.current.mutate("i1");
    });
    await waitFor(() => expect(person.result.current.isSuccess).toBe(true));
    expect(apiDelete).toHaveBeenLastCalledWith("/api/identities/i1/voiceprint");

    // Only `clusters.centroid` moved, so meetings and identities stay valid.
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.voiceprints.all });
    expect(invalidate).not.toHaveBeenCalledWith({ queryKey: queryKeys.meetings.all });
    expect(invalidate).not.toHaveBeenCalledWith({ queryKey: queryKeys.identities.all });
  });
});
