// End-to-end: the real React app -> the real core (scripted engine) -> the real orchestrator +
// persistence, driven in Chromium. Covers the documented flow: load with the session token, start a
// recording, watch the canned transcript stream in live over the WebSocket, stop, reopen from the
// Library, rename a speaker, and generate notes. The transcript/notes text is fixed by the scripted
// engine (hearsay_backends::build_scripted_engine), so the assertions are exact.
import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

import { HANDSHAKE_PATH } from "./paths";

// The core writes {port, token} to the handshake file just after it binds. The webServer readiness is
// a TCP accept, which can win the race against that write, so poll briefly for the token.
async function readToken(): Promise<string> {
  for (let attempt = 0; attempt < 80; attempt++) {
    try {
      const { token } = JSON.parse(readFileSync(HANDSHAKE_PATH, "utf8")) as { token?: string };
      if (token) return token;
    } catch {
      // handshake not written yet
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`no session token in ${HANDSHAKE_PATH} after 20s`);
}

// A per-run title so the Library lookup is unambiguous even though the scripted DB persists between
// runs (the config keeps its side-effects non-destructive — see playwright.config.ts).
const TITLE = `E2E Meeting ${Date.now()}`;

test("record -> live transcript -> stop -> library -> rename speaker -> notes", async ({ page }) => {
  const token = await readToken();
  // Dev serves the app via vite; the token rides in the query param (token.ts falls back to it).
  await page.goto(`/?token=${token}`);

  // Start a recording from the nav-rail popover.
  await page.getByRole("button", { name: "New recording" }).click();
  await page.getByLabel("Recording name").fill(TITLE);
  await page.getByRole("button", { name: "Start recording" }).click();

  // The live view: the canned transcript streams in over the WebSocket *while recording*. The Me turn
  // finalizes ~0.8s in, the Them turn ~1.2s in; both must appear before we stop.
  await expect(page.getByText("LIVE TRANSCRIPT", { exact: true })).toBeVisible();
  await expect(page.getByText("hello there")).toBeVisible();
  await expect(page.getByText("hi everyone, thanks for joining")).toBeVisible();
  // Them was diarized to a Speaker 1 cluster; Me is the local channel.
  await expect(page.getByText("Speaker 1").first()).toBeVisible();

  // Stop (the live control is labeled "End"), then wait for the finalized detail view to appear (the
  // meeting re-renders as TranscriptView once its status flips to finalized).
  await page.getByRole("button", { name: "End" }).click();
  await expect(page.getByRole("button", { name: "Speaker 1", exact: true })).toBeVisible();

  // Find the finalized meeting in the Library and reopen it from there.
  await page.getByRole("button", { name: "Meetings" }).click();
  await page.locator("button.library__row-open").filter({ hasText: TITLE }).click();

  // Rename the diarized speaker via the inline chip editor.
  await page.getByRole("button", { name: "Speaker 1", exact: true }).click();
  await page.getByLabel("Rename Speaker 1").fill("Alice");
  await page.locator("button.speaker-chip__save").click();
  // The chip (and the relabeled transcript lines) now read "Alice"; "Speaker 1" is gone.
  await expect(page.locator(".speaker-chip__name").filter({ hasText: "Alice" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Speaker 1", exact: true })).toHaveCount(0);

  // Generate notes: the button is enabled because a (stub) notes model resolves, and the scripted
  // summarizer returns a fixed summary.
  await page.getByRole("button", { name: "Generate notes" }).click();
  await expect(page.getByText("Scripted summary for the end-to-end test.")).toBeVisible();
});
