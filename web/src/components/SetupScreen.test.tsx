import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { beforeEach, expect, it, vi } from "vitest";

import { server } from "../test/server";

vi.mock("../api/token", () => ({ getToken: () => "test-token" }));

import { SetupScreen } from "./SetupScreen";

const idle = {
  required: true,
  status: "idle",
  message: null,
  steps: [
    {
      id: "live",
      label: "Speech models",
      status: "pending",
      downloaded_bytes: 0,
      total_bytes: 1_048_576 * 1024,
    },
    {
      id: "refine",
      label: "Refine model",
      status: "pending",
      downloaded_bytes: 0,
      total_bytes: 1_048_576 * 512,
    },
  ],
};

const catalog = {
  models_dir: "/models",
  items: [
    {
      id: "qwen3-4b",
      name: "Qwen3-4B",
      size_bytes: 1_048_576 * 2048,
      license: "Apache-2.0",
      context: "256K",
      note: "Best quality",
      recommended: true,
      installed: false,
    },
  ],
};

function renderSetup() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <SetupScreen />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  server.use(
    http.get("/api/setup", () => HttpResponse.json(idle)),
    http.get("/api/models/catalog", () => HttpResponse.json(catalog)),
  );
});

it("states the download size and lists each missing model", async () => {
  renderSetup();
  // 1.0 GB live + 512.0 MB refine, with no notes model selected.
  expect(await screen.findByText(/one-time download of about 1\.5 GB/)).toBeTruthy();
  expect(screen.getByText("Speech models")).toBeTruthy();
  expect(screen.getByText("Refine model")).toBeTruthy();
});

it("adds the notes model to the total only when it is opted into", async () => {
  renderSetup();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("checkbox"));
  // The 2.0 GB notes model joins the 1.5 GB of required models.
  expect(await screen.findByText(/one-time download of about 3\.5 GB/)).toBeTruthy();
});

it("sends the picked notes model when starting, and nothing when it is not opted into", async () => {
  const bodies: unknown[] = [];
  server.use(
    http.post("/api/setup", async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json({ ...idle, status: "running" });
    }),
  );
  renderSetup();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Download models" }));
  await waitFor(() => expect(bodies).toEqual([{ notes_model_id: null }]));
});

it("surfaces a failure and offers a retry that resumes", async () => {
  server.use(
    http.get("/api/setup", () =>
      HttpResponse.json({ ...idle, status: "error", message: "network unreachable" }),
    ),
  );
  renderSetup();
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toMatch(/network unreachable/);
  expect(alert.textContent).toMatch(/resume where they stopped/);
  expect((screen.getByRole("button", { name: "Try again" }) as HTMLButtonElement).disabled).toBe(
    false,
  );
});
