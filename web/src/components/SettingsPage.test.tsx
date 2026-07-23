import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { server } from "../test/server";

vi.mock("../api/token", () => ({ getToken: () => "test-token" }));
// The Danger Zone uses Tauri IPC (desktop only); stub it so the import is inert under test.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import SettingsPage from "./SettingsPage";

// Only the fields the mounted panels read; the Models panel guards on `models` + `models_info`.
const settingsFixture = {
  recording: {
    record: true,
    inactivity_prompt_enabled: true,
    inactivity_auto_end_enabled: true,
    inactivity_prompt_minutes: 5,
    inactivity_end_minutes: 10,
  },
  speakers: { auto_refine: false, recognition_threshold: 0.6 },
  storage: {},
  storage_info: {},
  models: { refine_model: "/models/ggml.bin", notes_enabled: false, notes_model: "", notes_prompt: "" },
  models_info: {
    default_notes_model: "",
    default_notes_prompt: "Summarize the transcript.",
    default_refine_model: "/default.bin",
    notes_model_exists: false,
    refine_model_exists: true,
  },
  about: {},
};

function renderSettings() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <SettingsPage onClose={vi.fn()} />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  server.use(
    http.get("/api/settings", () => HttpResponse.json(settingsFixture)),
    http.get("/api/models/catalog", () => HttpResponse.json({ items: [] })),
    http.get("/api/models/download", () =>
      HttpResponse.json({ status: "idle", model_id: null, progress: 0, error: null }),
    ),
  );
});

describe("SettingsPage models validation", () => {
  it("surfaces the server 422 when saving an invalid refine model path", async () => {
    const user = userEvent.setup();
    server.use(
      http.put("/api/settings/models", () =>
        HttpResponse.json({ detail: "refine model not found: /bad/path" }, { status: 422 }),
      ),
    );

    renderSettings();

    await user.click(screen.getByRole("button", { name: "Models" }));
    const input = await screen.findByRole("textbox", { name: "Refine transcription model path" });
    await user.clear(input);
    // The input commits on Enter (its onKeyDown), which triggers the save.
    await user.type(input, "/bad/path{Enter}");

    expect(await screen.findByText("refine model not found: /bad/path")).toBeTruthy();
  });
});
