import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { server } from "../test/server";

vi.mock("../api/token", () => ({ getToken: () => "test-token" }));
// The Danger Zone uses Tauri IPC (desktop only); stub it so the import is inert under test.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { StorageSettings } from "../api/types";

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
  storage: { output_dir: "/recordings", compress_audio: true, compress_after_days: 7 },
  storage_info: {
    output_dir: "/recordings",
    database_path: "/db/hearsay.db",
    tracked_bytes: 3_221_225_472,
    meeting_count: 12,
    uncompressed_bytes: 2_147_483_648,
  },
  models: { notes_enabled: false, notes_model: "", notes_prompt: "" },
  models_info: {
    default_notes_model: "",
    default_notes_prompt: "Summarize the transcript.",
    notes_model_exists: false,
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

describe("SettingsPage models panel", () => {
  it("saves the notes toggle with only the notes fields", async () => {
    const user = userEvent.setup();
    const bodies: unknown[] = [];
    server.use(
      http.put("/api/settings/models", async ({ request }) => {
        const body = await request.json();
        bodies.push(body);
        return HttpResponse.json(body);
      }),
    );

    renderSettings();

    await user.click(screen.getByRole("button", { name: "Models" }));
    await user.click(await screen.findByRole("checkbox", { name: /Summarize meetings/ }));

    await waitFor(() =>
      expect(bodies).toEqual([{ notes_enabled: true, notes_model: "", notes_prompt: "" }]),
    );
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

describe("SettingsPage storage", () => {
  it("renders the archival policy and what it would reclaim", async () => {
    const user = userEvent.setup();
    renderSettings();
    await user.click(await screen.findByRole("button", { name: "Storage" }));

    const toggle = await screen.findByRole("checkbox", {
      name: /Compress audio from older meetings/,
    });
    expect((toggle as HTMLInputElement).checked).toBe(true);
    const days = (await screen.findByLabelText("Compress after days")) as HTMLInputElement;
    expect(days.value).toBe("7");
    // 2 GiB of wav, which compresses to roughly a third.
    expect(await screen.findByText(/2\.0 GB — roughly 682\.7 MB once compressed/)).toBeTruthy();
  });

  it("sends the whole section when only the folder changes", async () => {
    const user = userEvent.setup();
    let body: StorageSettings | undefined;
    server.use(
      http.put("/api/settings/storage", async ({ request }) => {
        body = (await request.json()) as StorageSettings;
        return HttpResponse.json(body);
      }),
    );
    renderSettings();
    await user.click(await screen.findByRole("button", { name: "Storage" }));

    const input = await screen.findByLabelText("Default recordings location");
    await user.clear(input);
    await user.type(input, "/elsewhere");
    await user.click(screen.getByRole("button", { name: "Save" }));

    // The PUT full-replaces the section: omitting the archival fields here would silently switch
    // compression off as a side effect of moving the folder.
    await vi.waitFor(() =>
      expect(body).toEqual({
        output_dir: "/elsewhere",
        compress_audio: true,
        compress_after_days: 7,
      }),
    );
  });

  it("sends the whole section when only the toggle changes", async () => {
    const user = userEvent.setup();
    let body: StorageSettings | undefined;
    server.use(
      http.put("/api/settings/storage", async ({ request }) => {
        body = (await request.json()) as StorageSettings;
        return HttpResponse.json(body);
      }),
    );
    renderSettings();
    await user.click(await screen.findByRole("button", { name: "Storage" }));

    await user.click(
      await screen.findByRole("checkbox", { name: /Compress audio from older meetings/ }),
    );

    await vi.waitFor(() =>
      expect(body).toEqual({
        output_dir: "/recordings",
        compress_audio: false,
        compress_after_days: 7,
      }),
    );
  });
});

describe("SettingsPage compress now", () => {
  it("starts a pass and shows progress, then the result", async () => {
    const user = userEvent.setup();
    let started = false;
    server.use(
      http.get("/api/settings/storage/compress", () =>
        HttpResponse.json(
          started
            ? { running: true, total: 4, done: 1, compressed: 1, failed: 0, reclaimed_bytes: 1024 }
            : { running: false, total: 0, done: 0, compressed: 0, failed: 0, reclaimed_bytes: 0 },
        ),
      ),
      http.post("/api/settings/storage/compress", () => {
        started = true;
        return HttpResponse.json(
          { running: true, total: 4, done: 0, compressed: 0, failed: 0, reclaimed_bytes: 0 },
          { status: 202 },
        );
      }),
    );
    renderSettings();
    await user.click(await screen.findByRole("button", { name: "Storage" }));

    await user.click(await screen.findByRole("button", { name: "Compress now" }));

    // The button reflects the in-flight pass rather than looking idle while work happens.
    const busy = (await screen.findByRole("button", { name: "Compressing…" })) as HTMLButtonElement;
    expect(busy.disabled).toBe(true);
    expect(await screen.findByText(/of 4 meetings/)).toBeTruthy();
  });

  it("surfaces the server's refusal while a meeting is recording", async () => {
    const user = userEvent.setup();
    server.use(
      http.get("/api/settings/storage/compress", () =>
        HttpResponse.json({
          running: false,
          total: 0,
          done: 0,
          compressed: 0,
          failed: 0,
          reclaimed_bytes: 0,
        }),
      ),
      http.post("/api/settings/storage/compress", () =>
        HttpResponse.json(
          { detail: "a meeting is recording; archiving would compete with it" },
          { status: 409 },
        ),
      ),
    );
    renderSettings();
    await user.click(await screen.findByRole("button", { name: "Storage" }));
    await user.click(await screen.findByRole("button", { name: "Compress now" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/a meeting is recording/);
  });

  it("offers nothing to do when everything is already compressed", async () => {
    const user = userEvent.setup();
    server.use(
      http.get("/api/settings", () =>
        HttpResponse.json({
          ...settingsFixture,
          storage_info: { ...settingsFixture.storage_info, uncompressed_bytes: 0 },
        }),
      ),
      http.get("/api/settings/storage/compress", () =>
        HttpResponse.json({
          running: false,
          total: 0,
          done: 0,
          compressed: 0,
          failed: 0,
          reclaimed_bytes: 0,
        }),
      ),
    );
    renderSettings();
    await user.click(await screen.findByRole("button", { name: "Storage" }));

    const button = (await screen.findByRole("button", { name: "Compress now" })) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(await screen.findByText(/already compressed/)).toBeTruthy();
  });
});
