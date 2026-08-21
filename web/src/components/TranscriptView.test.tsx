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
  refine_incomplete: false,
  created_at: "2026-07-21T10:00:00Z",
  updated_at: "2026-07-21T10:00:00Z",
  started_at: "2026-07-21T10:00:00Z",
  ended_at: "2026-07-21T10:42:00Z",
};

function segment(
  text: string,
  edited: boolean,
  label = "Speaker 1",
  clusterId: string | null = "c1",
  stream: "me" | "them" = "them",
): SegmentRead {
  return {
    id: "seg-1",
    cluster_id: clusterId,
    edited,
    end_s: 4,
    speaker_label: label,
    start_s: 2,
    stream,
    text,
  };
}

function renderView(override: Partial<MeetingRead> = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(TranscriptView, { meeting: { ...meeting, ...override } }),
    ),
  );
}

let currentText: string;
let currentLabel: string;
let currentCluster: string | null;
let edited: boolean;

beforeEach(() => {
  currentText = "original text";
  currentLabel = "Speaker 1";
  currentCluster = "c1";
  edited = false;
  server.use(
    http.get("/api/meetings/:id/segments", () =>
      HttpResponse.json({
        total: 1,
        page: 1,
        page_size: 200,
        items: [segment(currentText, edited, currentLabel, currentCluster)],
      }),
    ),
    http.get("/api/folders", () =>
      HttpResponse.json({ total: 0, page: 1, page_size: 200, items: [] }),
    ),
    // TranscriptView now fetches the meeting's speakers + known identities itself (for the reassign
    // popover), so these must be mocked even for the plain edit flow.
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
    http.patch("/api/meetings/:id/segments/:segmentId", async ({ request }) => {
      const body = (await request.json()) as { text: string };
      currentText = body.text;
      edited = true;
      return HttpResponse.json(segment(currentText, edited, currentLabel, currentCluster));
    }),
    http.patch("/api/meetings/:id/segments/:segmentId/speaker", async ({ request }) => {
      const body = (await request.json()) as { cluster_id?: string; display_name?: string };
      if (body.display_name) {
        currentLabel = body.display_name.trim();
        currentCluster = "c-new";
      } else if (body.cluster_id) {
        currentCluster = body.cluster_id;
        currentLabel = body.cluster_id === "c2" ? "Speaker 2" : "Speaker 1";
      }
      edited = true;
      return HttpResponse.json(segment(currentText, edited, currentLabel, currentCluster));
    }),
  );
});

describe("TranscriptView truncated-refine notice", () => {
  it("warns with the coverage percentage and offers a re-refine", async () => {
    renderView({ refine_incomplete: true, refine_coverage: 0.29 });

    const notice = await screen.findByRole("alert");
    expect(notice.textContent).toContain("29%");
    expect(screen.getByRole("button", { name: "Refine again" })).toBeTruthy();
  });

  it("stays hidden for a healthy refine", async () => {
    renderView({ refine_incomplete: false, refine_coverage: 0.99 });

    expect(await screen.findByText("original text")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Refine again" })).toBeNull();
  });
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

describe("TranscriptView speaker reassignment", () => {
  it("reassigns a line to an existing speaker", async () => {
    const user = userEvent.setup();
    renderView();

    expect(await screen.findByText("original text")).toBeTruthy();
    expect(screen.getByText("Speaker 1")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Reassign speaker" }));
    // The line's current speaker is offered but disabled (you can't reassign to where it already is).
    const current = await screen.findByRole("button", { name: /Speaker 1 \(current\)/ });
    expect(current).toHaveProperty("disabled", true);

    await user.click(await screen.findByRole("button", { name: /Speaker 2/ }));

    // After the PATCH + refetch, the line carries the new speaker and the popover has closed.
    expect(await screen.findByText("Speaker 2")).toBeTruthy();
    // The name field is a combobox (its datalist `list=` attribute upgrades the role from textbox).
    expect(screen.queryByRole("combobox", { name: "New speaker name" })).toBeNull();
  });

  it("assigns a line to a brand-new named speaker", async () => {
    const user = userEvent.setup();
    renderView();

    expect(await screen.findByText("original text")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Reassign speaker" }));
    const input = await screen.findByRole("combobox", { name: "New speaker name" });
    await user.type(input, "Dana");
    await user.click(screen.getByRole("button", { name: "Add" }));

    expect(await screen.findByText("Dana")).toBeTruthy();
  });

  it("offers no speaker reassignment on a Me line", async () => {
    server.use(
      http.get("/api/meetings/:id/segments", () =>
        HttpResponse.json({
          total: 1,
          page: 1,
          page_size: 200,
          items: [segment("mine", false, "Me", null, "me")],
        }),
      ),
    );
    renderView();

    expect(await screen.findByText("mine")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Edit this line" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reassign speaker" })).toBeNull();
  });
});
