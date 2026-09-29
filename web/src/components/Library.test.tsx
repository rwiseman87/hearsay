import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import type { ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { MeetingRead } from "../api/types";
import { server } from "../test/server";

// The fetch wrapper reads the session token; the Library owns its own queries, so the meetings
// list, the sidebar counts, and the folder tree are all served from here.
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

// Serve a page of `items` out of a `total`-sized result set, echoing the request's page size.
function page(items: MeetingRead[], total = items.length, pageNumber = 1, pageSize = 50) {
  return HttpResponse.json({ total, page: pageNumber, page_size: pageSize, items });
}

function renderLibrary(onSelect: (id: string) => void = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrap = (node: ReactNode) => <QueryClientProvider client={client}>{node}</QueryClientProvider>;
  return render(wrap(<Library onSelect={onSelect} />));
}

beforeEach(() => {
  server.use(
    http.get("/api/folders", () =>
      HttpResponse.json({ total: 0, page: 1, page_size: 200, items: [] }),
    ),
    http.get("/api/meetings/counts", () =>
      HttpResponse.json({ total: 0, unfiled: 0, folders: [] }),
    ),
    http.get("/api/meetings", () => page([])),
  );
});

describe("Library", () => {
  it("renders the meetings and selects one on click", async () => {
    const onSelect = vi.fn();
    server.use(
      http.get("/api/meetings", () => page([meeting("m-1", "Standup"), meeting("m-2", "Roadmap review")])),
    );
    renderLibrary(onSelect);

    expect(await screen.findByText("Standup")).toBeTruthy();
    expect(screen.getByText("Roadmap review")).toBeTruthy();

    await userEvent.setup().click(screen.getByText("Standup"));
    expect(onSelect).toHaveBeenCalledWith("m-1");
  });

  it("takes the sidebar counts from the server, not from the listed page", async () => {
    server.use(
      http.get("/api/meetings/counts", () =>
        HttpResponse.json({
          total: 87,
          unfiled: 1,
          folders: [{ folder_id: "f-1", meetings: 22 }],
        }),
      ),
      http.get("/api/folders", () =>
        HttpResponse.json({
          total: 1,
          page: 1,
          page_size: 200,
          items: [
            {
              id: "f-1",
              name: "PBS",
              parent_id: null,
              created_at: "2026-07-23T10:00:00Z",
              updated_at: "2026-07-23T10:00:00Z",
            },
          ],
        }),
      ),
      // One page of 50 out of 87: the badges must still report the whole database.
      http.get("/api/meetings", () =>
        page(
          Array.from({ length: 50 }, (_, i) => meeting(`m-${i}`, `Meeting ${i}`)),
          87,
        ),
      ),
    );
    renderLibrary();

    await vi.waitFor(() =>
      expect(screen.getByRole("button", { name: /All meetings/ }).textContent).toContain("87"),
    );
    expect(screen.getByRole("button", { name: /Unfiled/ }).textContent).toContain("1");
    expect(await screen.findByText("PBS")).toBeTruthy();
    expect(screen.getByText("22")).toBeTruthy();
  });

  it("pages through the meetings server-side", async () => {
    const requested: string[] = [];
    server.use(
      http.get("/api/meetings", ({ request }) => {
        const url = new URL(request.url);
        requested.push(url.searchParams.get("page") ?? "");
        const pageNumber = Number(url.searchParams.get("page") ?? "1");
        return page([meeting(`m-${pageNumber}`, `On page ${pageNumber}`)], 87, pageNumber);
      }),
    );
    renderLibrary();

    expect(await screen.findByText("On page 1")).toBeTruthy();
    // 87 meetings at 50 a page is two pages.
    expect(screen.getByRole("button", { name: "Page 2" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Page 3" })).toBeNull();

    await userEvent.setup().click(screen.getByRole("button", { name: "Page 2" }));

    expect(await screen.findByText("On page 2")).toBeTruthy();
    expect(requested).toContain("2");
  });

  it("sends the folder filter, the title search, and the sort to the server", async () => {
    const requests: URL[] = [];
    server.use(
      http.get("/api/folders", () =>
        HttpResponse.json({
          total: 1,
          page: 1,
          page_size: 200,
          items: [
            {
              id: "f-1",
              name: "PBS",
              parent_id: null,
              created_at: "2026-07-23T10:00:00Z",
              updated_at: "2026-07-23T10:00:00Z",
            },
          ],
        }),
      ),
      http.get("/api/meetings", ({ request }) => {
        requests.push(new URL(request.url));
        return page([meeting("m-1", "Standup")]);
      }),
    );
    const user = userEvent.setup();
    renderLibrary();
    await screen.findByText("Standup");

    await user.click(await screen.findByText("PBS"));
    await vi.waitFor(() =>
      expect(requests.at(-1)?.searchParams.get("folder_id")).toBe("f-1"),
    );

    await user.click(screen.getByRole("button", { name: /Unfiled/ }));
    await vi.waitFor(() => expect(requests.at(-1)?.searchParams.get("unfiled")).toBe("true"));

    await user.type(screen.getByLabelText("Search meetings by title"), "kickoff");
    await vi.waitFor(() => expect(requests.at(-1)?.searchParams.get("q")).toBe("kickoff"));

    await user.selectOptions(screen.getByLabelText("Sort meetings"), "oldest");
    await vi.waitFor(() => expect(requests.at(-1)?.searchParams.get("sort")).toBe("oldest"));
  });

  it("deletes a meeting after confirmation", async () => {
    const user = userEvent.setup();
    let deletedId: string | undefined;
    server.use(
      http.get("/api/meetings", () => page([meeting("m-1", "Standup")])),
      http.delete("/api/meetings/:id", ({ params }) => {
        deletedId = params.id as string;
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderLibrary();
    await screen.findByText("Standup");

    await user.click(screen.getByRole("button", { name: "Delete Standup" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete Standup" }));

    await vi.waitFor(() => expect(deletedId).toBe("m-1"));
  });

  it("surfaces the load error", async () => {
    server.use(
      http.get("/api/meetings", () =>
        HttpResponse.json({ detail: "could not load meetings" }, { status: 500 }),
      ),
    );
    renderLibrary();
    expect(await screen.findByText(/could not load meetings/)).toBeTruthy();
  });
});
