import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { createElement } from "react";
import { beforeEach, expect, it, vi } from "vitest";

import type { MeetingRead, SegmentRead } from "../api/types";
import { queryKeys } from "../api/queryKeys";
import { server } from "../test/server";

vi.mock("../api/token", () => ({ getToken: () => "test-token" }));
vi.mock("./NotesPanel", () => ({ NotesPanel: () => null }));
vi.mock("./UserNotesSection", () => ({ UserNotesSection: () => null }));

// A stand-in for the speaker chips that exposes the filter props as plain buttons, so this file
// tests what TranscriptView does with the filter rather than how the chips look.
vi.mock("./SpeakerPanel", () => ({
  ME_FILTER_KEY: "me",
  SpeakerPanel: ({
    onToggle,
    onClear,
    hasMe,
    visibleCount,
    totalCount,
  }: {
    onToggle: (key: string) => void;
    onClear: () => void;
    hasMe: boolean;
    visibleCount: number;
    totalCount: number;
  }) =>
    createElement(
      "div",
      null,
      createElement("button", { onClick: () => onToggle("c1") }, "toggle-c1"),
      createElement("button", { onClick: () => onToggle("me") }, "toggle-me"),
      createElement("button", { onClick: onClear }, "clear-filter"),
      createElement("span", null, `${visibleCount}/${totalCount}`),
      hasMe ? createElement("span", null, "has-me") : null,
    ),
}));

import { TranscriptView } from "./TranscriptView";

const meeting: MeetingRead = {
  id: "m-1",
  title: "Weekly Sync",
  folder: "sync",
  folder_id: null,
  status: "finalized",
  created_at: "2026-07-21T10:00:00Z",
  updated_at: "2026-07-21T10:00:00Z",
  started_at: "2026-07-21T10:00:00Z",
  ended_at: "2026-07-21T10:42:00Z",
};

const SEGMENTS: SegmentRead[] = [
  {
    id: "s1",
    cluster_id: "c1",
    edited: false,
    start_s: 0,
    end_s: 1,
    speaker_label: "Speaker 1",
    stream: "them",
    text: "apple from one",
  },
  {
    id: "s2",
    cluster_id: "c2",
    edited: false,
    start_s: 1,
    end_s: 2,
    speaker_label: "Speaker 2",
    stream: "them",
    text: "apple from two",
  },
  {
    id: "s3",
    cluster_id: null,
    edited: false,
    start_s: 2,
    end_s: 3,
    speaker_label: "Me",
    stream: "me",
    text: "apple from me",
  },
];

function renderView() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(QueryClientProvider, { client }, createElement(TranscriptView, { meeting })),
  );
  return client;
}

beforeEach(() => {
  server.use(
    http.get("/api/meetings/:id/segments", () =>
      HttpResponse.json({ total: 3, page: 1, page_size: 200, items: SEGMENTS }),
    ),
    http.get("/api/folders", () =>
      HttpResponse.json({ total: 0, page: 1, page_size: 200, items: [] }),
    ),
    http.get("/api/meetings/:id/speakers", () =>
      HttpResponse.json({
        total: 2,
        page: 1,
        page_size: 2,
        items: [
          { id: "c1", ordinal: 1, label: "Speaker 1", identity_id: null, locked: false },
          { id: "c2", ordinal: 2, label: "Speaker 2", identity_id: null, locked: false },
        ],
      }),
    ),
    http.get("/api/identities", () =>
      HttpResponse.json({ total: 0, page: 1, page_size: 50, items: [] }),
    ),
  );
});

it("shows only the selected speaker's lines and restores them on clear", async () => {
  const user = userEvent.setup();
  renderView();
  expect(await screen.findByText("apple from one")).toBeTruthy();
  expect(screen.getByText("apple from two")).toBeTruthy();
  expect(screen.getByText("apple from me")).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "toggle-c1" }));
  expect(screen.getByText("apple from one")).toBeTruthy();
  expect(screen.queryByText("apple from two")).toBeNull();
  expect(screen.queryByText("apple from me")).toBeNull();
  expect(screen.getByText("1/3")).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "clear-filter" }));
  expect(screen.getByText("apple from two")).toBeTruthy();
  expect(screen.getByText("3/3")).toBeTruthy();
});

it("filters to Me, which has no cluster of its own", async () => {
  const user = userEvent.setup();
  renderView();
  expect(await screen.findByText("has-me")).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "toggle-me" }));
  expect(screen.getByText("apple from me")).toBeTruthy();
  expect(screen.queryByText("apple from one")).toBeNull();
});

it("selects several speakers at once", async () => {
  const user = userEvent.setup();
  renderView();
  expect(await screen.findByText("apple from one")).toBeTruthy();

  await user.click(screen.getByRole("button", { name: "toggle-c1" }));
  await user.click(screen.getByRole("button", { name: "toggle-me" }));
  expect(screen.getByText("apple from one")).toBeTruthy();
  expect(screen.getByText("apple from me")).toBeTruthy();
  expect(screen.queryByText("apple from two")).toBeNull();
  expect(screen.getByText("2/3")).toBeTruthy();
});

it("scopes the find counter to the visible lines", async () => {
  const user = userEvent.setup();
  renderView();
  expect(await screen.findByText("apple from one")).toBeTruthy();

  // "apple" is in all three lines ...
  await user.type(screen.getByLabelText("Find in transcript"), "apple");
  expect(screen.getByText("1/3")).toBeTruthy();

  // ... but the counter must follow the filter, not the whole transcript.
  await user.click(screen.getByRole("button", { name: "toggle-c1" }));
  expect(screen.getByText("1/1")).toBeTruthy();
});

it("drops filter entries whose cluster disappears", async () => {
  const user = userEvent.setup();
  const client = renderView();
  expect(await screen.findByText("apple from one")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "toggle-c1" }));
  expect(screen.getByText("1/3")).toBeTruthy();

  // A merge (or a re-diarize) deletes clusters and invalidates the speakers query. If the now-dead
  // id stayed in the filter, the transcript would silently empty with nothing explaining why — so
  // reproduce exactly that: c1 is gone, and its lines came across to c2.
  server.use(
    http.get("/api/meetings/:id/speakers", () =>
      HttpResponse.json({
        total: 1,
        page: 1,
        page_size: 1,
        items: [{ id: "c2", ordinal: 2, label: "Speaker 2", identity_id: null, locked: false }],
      }),
    ),
  );
  await client.invalidateQueries({ queryKey: queryKeys.meetings.speakers(meeting.id) });

  expect(await screen.findByText("3/3")).toBeTruthy();
  expect(screen.getByText("apple from two")).toBeTruthy();
});
