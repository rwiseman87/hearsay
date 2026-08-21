import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import type { ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { MeetingRead } from "../api/types";
import { server } from "../test/server";

// The fetch wrapper reads the session token; the meeting list itself is passed in as a prop, so only
// the folder fetch (+ the delete mutation) touches the network.
vi.mock("../api/token", () => ({ getToken: () => "test-token" }));

import { Library } from "./Library";

function meeting(id: string, title: string): MeetingRead {
  return {
    id,
    title,
    folder: "",
    folder_id: null,
    status: "finalized",
    refine_incomplete: false,
    created_at: "2026-07-23T10:00:00Z",
    updated_at: "2026-07-23T10:00:00Z",
    started_at: "2026-07-23T10:00:00Z",
    ended_at: "2026-07-23T10:30:00Z",
  };
}

function renderLibrary(props: {
  meetings?: MeetingRead[];
  isLoading?: boolean;
  error?: unknown;
  onSelect?: (id: string) => void;
}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrap = (node: ReactNode) => <QueryClientProvider client={client}>{node}</QueryClientProvider>;
  return render(
    wrap(
      <Library
        meetings={props.meetings ?? []}
        isLoading={props.isLoading ?? false}
        error={props.error ?? null}
        onSelect={props.onSelect ?? vi.fn()}
      />,
    ),
  );
}

beforeEach(() => {
  server.use(
    http.get("/api/folders", () =>
      HttpResponse.json({ total: 0, page: 1, page_size: 200, items: [] }),
    ),
  );
});

describe("Library", () => {
  it("renders the meetings and selects one on click", async () => {
    const onSelect = vi.fn();
    renderLibrary({
      meetings: [meeting("m-1", "Standup"), meeting("m-2", "Roadmap review")],
      onSelect,
    });

    expect(await screen.findByText("Standup")).toBeTruthy();
    expect(screen.getByText("Roadmap review")).toBeTruthy();

    await userEvent.setup().click(screen.getByText("Standup"));
    expect(onSelect).toHaveBeenCalledWith("m-1");
  });

  it("deletes a meeting after confirmation", async () => {
    const user = userEvent.setup();
    let deletedId: string | undefined;
    server.use(
      http.delete("/api/meetings/:id", ({ params }) => {
        deletedId = params.id as string;
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderLibrary({ meetings: [meeting("m-1", "Standup")] });
    await screen.findByText("Standup");

    await user.click(screen.getByRole("button", { name: "Delete Standup" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete Standup" }));

    await vi.waitFor(() => expect(deletedId).toBe("m-1"));
  });

  it("surfaces the load error", async () => {
    renderLibrary({ error: new Error("could not load meetings") });
    expect(await screen.findByText("could not load meetings")).toBeTruthy();
  });
});
