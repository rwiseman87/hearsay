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

describe("SettingsPage voices panel", () => {
  const alice = {
    identity_id: "i1",
    display_name: "Alice",
    email: null,
    sample_count: 2,
    active_count: 2,
    last_heard: "2026-08-01T10:00:00Z",
    samples: [
      {
        id: "c1",
        meeting_id: "m1",
        meeting_title: "Weekly Sync",
        started_at: "2026-08-01T10:00:00Z",
        locked: true,
        dimension: 256,
      },
      {
        id: "c2",
        meeting_id: "m2",
        meeting_title: "Kickoff",
        started_at: "2026-07-02T10:00:00Z",
        locked: true,
        dimension: 256,
      },
    ],
  };
  // A sample exists but was never manually confirmed, so it is not a recognition candidate.
  const bob = {
    identity_id: "i2",
    display_name: "Bob",
    email: null,
    sample_count: 1,
    active_count: 0,
    last_heard: "2026-07-02T10:00:00Z",
    samples: [
      {
        id: "c3",
        meeting_id: "m2",
        meeting_title: "Design review",
        started_at: "2026-07-02T10:00:00Z",
        locked: false,
        dimension: 256,
      },
    ],
  };

  const voiceprints = (...items: unknown[]) =>
    http.get("/api/voiceprints", () =>
      HttpResponse.json({ total: items.length, page: 1, page_size: 50, items }),
    );

  it("lists people and expands to their stored samples", async () => {
    const user = userEvent.setup();
    server.use(voiceprints(alice, bob));
    renderSettings();

    await user.click(screen.getByRole("button", { name: "Voices" }));
    expect(await screen.findByText("2 voices")).toBeTruthy();
    // Stored but unconfirmed, so it matches nothing yet — a distinct state from having no sample.
    expect(screen.getByText("Not in use")).toBeTruthy();

    // Samples load with the roster, so expanding is instant and needs no second endpoint.
    expect(screen.queryByText("Weekly Sync")).toBeNull();
    await user.click(screen.getByRole("button", { name: /Alice/ }));
    expect(screen.getByText("Weekly Sync")).toBeTruthy();
    expect(screen.getByText("Kickoff")).toBeTruthy();
  });

  it("removes a single sample without touching the person", async () => {
    const user = userEvent.setup();
    let deleted: string | null = null;
    server.use(
      voiceprints(alice),
      http.delete("/api/voiceprints/:clusterId", ({ params }) => {
        deleted = String(params.clusterId);
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderSettings();

    await user.click(screen.getByRole("button", { name: "Voices" }));
    await user.click(await screen.findByRole("button", { name: /Alice/ }));
    await user.click(screen.getAllByRole("button", { name: "Remove" })[0]);

    expect(deleted).toBe("c1");
  });

  it("forgets a whole voice behind a confirm step", async () => {
    const user = userEvent.setup();
    let forgot: string | null = null;
    server.use(
      voiceprints(alice),
      http.delete("/api/identities/:id/voiceprint", ({ params }) => {
        forgot = String(params.id);
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderSettings();

    await user.click(screen.getByRole("button", { name: "Voices" }));
    await user.click(await screen.findByRole("button", { name: "Forget voice" }));
    expect(forgot).toBeNull();
    await user.click(screen.getByRole("button", { name: "Confirm" }));

    expect(forgot).toBe("i1");
  });

  it("surfaces a name collision from the rename", async () => {
    const user = userEvent.setup();
    server.use(
      voiceprints(alice, bob),
      http.patch("/api/identities/:id", () =>
        HttpResponse.json({ detail: "another person already uses that name" }, { status: 409 }),
      ),
    );
    renderSettings();

    await user.click(screen.getByRole("button", { name: "Voices" }));
    // The field is revealed by Rename rather than sitting on every row; Alice is the first person.
    await user.click((await screen.findAllByRole("button", { name: "Rename" }))[0]);
    const input = screen.getByRole("textbox", { name: "Rename Alice" });
    await user.clear(input);
    await user.type(input, "Bob{Enter}");

    expect(await screen.findByText("another person already uses that name")).toBeTruthy();
  });
});
