import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { createElement } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { MeetingRead, SegmentRead } from "../api/types";
import { server } from "../test/server";

// Isolate the transcript + edit flow: stub the token (audio URL reads it) and the data-fetching child
// panels, so the test only needs the segments/folders endpoints plus the edit PATCH — the real fetch
// wrapper runs against MSW.
vi.mock("../api/token", () => ({ getToken: () => "test-token" }));
vi.mock("./NotesPanel", () => ({ NotesPanel: () => null }));
vi.mock("./SpeakerPanel", () => ({ SpeakerPanel: () => null }));
vi.mock("./UserNotesSection", () => ({ UserNotesSection: () => null }));

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

function segment(text: string, edited: boolean): SegmentRead {
  return {
    id: "seg-1",
    cluster_id: null,
    edited,
    end_s: 4,
    speaker_label: "Speaker 1",
    start_s: 2,
    stream: "them",
    text,
  };
}

function renderView() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    createElement(QueryClientProvider, { client }, createElement(TranscriptView, { meeting })),
  );
}

let currentText: string;
let edited: boolean;

beforeEach(() => {
  currentText = "original text";
  edited = false;
  server.use(
    http.get("/api/meetings/:id/segments", () =>
      HttpResponse.json({
        total: 1,
        page: 1,
        page_size: 200,
        items: [segment(currentText, edited)],
      }),
    ),
    http.get("/api/folders", () =>
      HttpResponse.json({ total: 0, page: 1, page_size: 200, items: [] }),
    ),
    http.patch("/api/meetings/:id/segments/:segmentId", async ({ request }) => {
      const body = (await request.json()) as { text: string };
      currentText = body.text;
      edited = true;
      return HttpResponse.json(segment(currentText, edited));
    }),
  );
});

describe("TranscriptView editing", () => {
  it("edits a transcript line and reflects the persisted text", async () => {
    const user = userEvent.setup();
    renderView();

    expect(await screen.findByText("original text")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Edit this line" }));
    const textarea = screen.getByRole("textbox", { name: "Edit transcript line" });
    await user.clear(textarea);
    await user.type(textarea, "corrected text");
    await user.click(screen.getByRole("button", { name: "Save" }));

    // After the PATCH + the invalidation-driven refetch, the corrected text shows and the inline
    // editor has closed.
    expect(await screen.findByText("corrected text")).toBeTruthy();
    expect(screen.queryByRole("textbox", { name: "Edit transcript line" })).toBeNull();
  });
});
