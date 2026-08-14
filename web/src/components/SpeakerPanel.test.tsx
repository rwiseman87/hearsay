import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { createElement } from "react";
import { beforeEach, expect, it, vi } from "vitest";

import { server } from "../test/server";
import { SpeakerPanel } from "./SpeakerPanel";

vi.mock("../api/token", () => ({ getToken: () => "test-token" }));

const SPEAKERS = [
  { id: "c1", ordinal: 1, label: "Speaker 1", identity_id: null, locked: false },
  { id: "c2", ordinal: 2, label: "Alice", identity_id: "i1", locked: true },
];

let merged: { clusterId: string; into: string } | null;
let renamed: string | null;

function renderPanel(
  props: Partial<Parameters<typeof SpeakerPanel>[0]> = {},
  onToggle = vi.fn(),
) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(SpeakerPanel, {
        meetingId: "m-1",
        live: false,
        selected: new Set<string>(),
        hasMe: true,
        onToggle,
        onClear: vi.fn(),
        visibleCount: 3,
        totalCount: 3,
        ...props,
      }),
    ),
  );
  return onToggle;
}

beforeEach(() => {
  merged = null;
  renamed = null;
  server.use(
    http.get("/api/meetings/:id/speakers", () =>
      HttpResponse.json({ total: 2, page: 1, page_size: 2, items: SPEAKERS }),
    ),
    http.get("/api/identities", () =>
      HttpResponse.json({ total: 0, page: 1, page_size: 50, items: [] }),
    ),
    http.put("/api/meetings/:id/speakers/:clusterId", async ({ request }) => {
      const body = (await request.json()) as { display_name: string };
      renamed = body.display_name;
      return HttpResponse.json({ ...SPEAKERS[0], label: body.display_name, locked: true });
    }),
    http.post("/api/meetings/:id/speakers/:clusterId/merge", async ({ params, request }) => {
      const body = (await request.json()) as { into: string };
      merged = { clusterId: String(params.clusterId), into: body.into };
      return HttpResponse.json({ total: 1, page: 1, page_size: 1, items: [SPEAKERS[1]] });
    }),
  );
});

it("toggles the speaker filter from the chip body", async () => {
  const user = userEvent.setup();
  const onToggle = renderPanel();

  const chip = await screen.findByRole("button", { name: "Speaker 1" });
  expect(chip.getAttribute("aria-pressed")).toBe("false");
  await user.click(chip);
  expect(onToggle).toHaveBeenCalledWith("c1");
});

it("marks the selected chip pressed and offers Me", async () => {
  renderPanel({ selected: new Set(["c1", "me"]) });

  const chip = await screen.findByRole("button", { name: "Speaker 1" });
  expect(chip.getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByRole("button", { name: "Me" })).toBeTruthy();
});

it("hides the Me chip when the meeting has no mic lines", async () => {
  renderPanel({ hasMe: false });

  expect(await screen.findByRole("button", { name: "Speaker 1" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Me" })).toBeNull();
});

it("renames from the chip menu", async () => {
  const user = userEvent.setup();
  renderPanel();

  await user.click(
    await screen.findByRole("button", { name: "Speaker actions for Speaker 1" }),
  );
  await user.click(screen.getByRole("button", { name: "Rename" }));
  await user.type(screen.getByLabelText("Rename Speaker 1"), "Dana");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(renamed).toBe("Dana");
});

it("merges into another speaker, but only after a confirm", async () => {
  const user = userEvent.setup();
  renderPanel();

  await user.click(
    await screen.findByRole("button", { name: "Speaker actions for Speaker 1" }),
  );
  await user.click(screen.getByRole("button", { name: "Merge into…" }));

  // Scoped to the popover: "Alice" also names her own chip out in the strip.
  const popover = within(document.querySelector(".speaker-chip__pop") as HTMLElement);
  // The list offers the other speakers, never the one being merged away.
  expect(popover.getByRole("button", { name: "Alice" })).toBeTruthy();
  expect(popover.queryByRole("button", { name: "Speaker 1" })).toBeNull();

  await user.click(popover.getByRole("button", { name: "Alice" }));
  // Picking a target only arms the merge; nothing is sent until it is confirmed.
  expect(merged).toBeNull();
  await user.click(screen.getByRole("button", { name: "Merge" }));

  expect(merged).toEqual({ clusterId: "c1", into: "c2" });
});

it("offers neither filtering nor merging while recording", async () => {
  const user = userEvent.setup();
  renderPanel({ live: true });

  // The label is still the rename affordance during a live meeting, as it has always been.
  const chip = await screen.findByRole("button", { name: "Speaker 1" });
  expect(chip.getAttribute("aria-pressed")).toBeNull();
  expect(screen.queryByRole("button", { name: /Speaker actions/ })).toBeNull();
  expect(screen.queryByRole("button", { name: "Me" })).toBeNull();

  await user.click(chip);
  expect(screen.getByLabelText("Rename Speaker 1")).toBeTruthy();
});
