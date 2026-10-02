// End-to-end: a meeting whose audio exists ONLY in the archived (compressed) form still serves and
// plays. This is the payoff test for storage archival — the sweep deletes the original WAV, so if
// the browser cannot decode what replaced it, meetings silently lose their playback.
//
// The fixture is produced by the real encoder (`make e2e` runs
// `cargo run -p hearsay-audio --example fixture`), not a stub, so this covers the actual bytes the
// sweep writes: the container, the declared duration, and the served content type.
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";

import { expect, test } from "@playwright/test";

import { FLAC_FIXTURE, HANDSHAKE_PATH, MEETINGS_DIR } from "./paths";

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

const TITLE = `E2E Archived ${Date.now()}`;

test("an archived meeting serves and plays its compressed audio", async ({ page, request }) => {
  expect(
    existsSync(FLAC_FIXTURE),
    `missing ${FLAC_FIXTURE} — run via 'make e2e', which builds it`,
  ).toBe(true);

  const token = await readToken();
  const auth = { Authorization: `Bearer ${token}` };

  // Record and stop a scripted meeting through the API; the UI flow is covered by meeting.spec.ts.
  const created = await request.post("/api/meetings", {
    headers: auth,
    data: { title: TITLE },
  });
  expect(created.ok()).toBe(true);
  const meeting = (await created.json()) as { id: string; folder: string };
  const stopped = await request.post(`/api/meetings/${meeting.id}/stop`, { headers: auth });
  expect(stopped.ok()).toBe(true);

  await expect
    .poll(
      async () => {
        const read = await request.get(`/api/meetings/${meeting.id}`, { headers: auth });
        return ((await read.json()) as { status: string }).status;
      },
      { timeout: 30_000 },
    )
    .toBe("finalized");

  // Put the meeting in the state the sweep leaves behind: the archived file, and no WAV beside it.
  const dir = path.join(MEETINGS_DIR, meeting.folder);
  mkdirSync(dir, { recursive: true });
  rmSync(path.join(dir, "audio.wav"), { force: true });
  copyFileSync(FLAC_FIXTURE, path.join(dir, "audio.flac"));

  // The route decodes the archived form and serves it as WAV, whose byte offsets map to time exactly.
  const audio = await request.get(`/api/meetings/${meeting.id}/audio?token=${token}`);
  expect(audio.status()).toBe(200);
  expect(audio.headers()["content-type"]).toBe("audio/wav");

  // Range requests still work, so the player can seek.
  const ranged = await request.get(`/api/meetings/${meeting.id}/audio?token=${token}`, {
    headers: { Range: "bytes=0-99" },
  });
  expect(ranged.status()).toBe(206);

  // And in the browser: open the meeting and confirm the player accepted it. The scrubber removes
  // itself on a decode error (`onError` clears `hasAudio`), so its presence is the real assertion,
  // and a non-zero duration proves the encoder wrote the sample count into the stream header —
  // without it the scrubber renders but cannot seek.
  await page.goto(`/?token=${token}`);
  await page.getByRole("button", { name: "Meetings" }).click();
  await page.locator("button.library__row-open").filter({ hasText: TITLE }).click();

  const player = page.locator(".detail__scrubber audio");
  await expect(player).toBeAttached();
  await expect
    .poll(async () => player.evaluate((el: HTMLAudioElement) => el.readyState), {
      timeout: 15_000,
    })
    .toBeGreaterThanOrEqual(1);
  const duration = await player.evaluate((el: HTMLAudioElement) => el.duration);
  expect(duration).toBeGreaterThan(1.5);
  expect(duration).toBeLessThan(2.5);
  await expect(page.locator(".detail__scrubber")).toBeVisible();
});
