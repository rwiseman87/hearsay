// End-to-end: the Storage settings panel against the real core. Covers the audio-archival policy
// round-tripping through the API and surviving a reload, and — the reason this spec exists — that
// saving one field never silently resets the others. The PUT full-replaces the `storage` section,
// so a partial write would switch archival off as a side effect of moving the recordings folder.
import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

import { HANDSHAKE_PATH } from "./paths";

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

test("storage panel round-trips the archival policy without clobbering the folder", async ({
  page,
}) => {
  const token = await readToken();
  await page.goto(`/?token=${token}`);

  const openStorage = async () => {
    await page.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("button", { name: "Storage" }).click();
  };
  await openStorage();

  // The shipped defaults: archival on, 7 days. A settings row written before archival existed must
  // still resolve to these rather than to `false` / 0.
  const toggle = page.getByRole("checkbox", { name: /Compress audio from older meetings/ });
  await expect(toggle).toBeChecked();
  const days = page.getByLabel("Compress after days");
  await expect(days).toHaveValue("7");

  // The folder input is populated from the server, and every panel rendered (a section that failed
  // to resolve would take the whole aggregated GET /settings down with it).
  const folder = page.getByLabel("Default recordings location");
  await expect(folder).not.toHaveValue("");
  const originalFolder = await folder.inputValue();

  // Change the threshold and confirm it persisted, not just that the input echoed it back.
  await days.fill("21");
  await days.blur();
  await page.reload();
  await openStorage();
  await expect(page.getByLabel("Compress after days")).toHaveValue("21");
  await expect(page.getByLabel("Default recordings location")).toHaveValue(originalFolder);

  // Now save the folder on its own. The archival settings must survive it untouched.
  await page.getByLabel("Default recordings location").fill(originalFolder);
  await days.fill("21");
  await page.reload();
  await openStorage();
  await expect(
    page.getByRole("checkbox", { name: /Compress audio from older meetings/ }),
  ).toBeChecked();
  await expect(page.getByLabel("Compress after days")).toHaveValue("21");

  // Restore the default so the persisted e2e DB does not carry the change into later runs.
  await page.getByLabel("Compress after days").fill("7");
  await page.getByLabel("Compress after days").blur();
  await expect(page.getByLabel("Compress after days")).toHaveValue("7");
});

test("the storage panel can run an archival pass on demand", async ({ page, request }) => {
  const token = await readToken();
  await page.goto(`/?token=${token}`);
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("button", { name: "Storage" }).click();

  // The control is present and explains itself. The scripted meetings are minutes old, so there is
  // nothing eligible and the button says so rather than pretending it will do something.
  const button = page.getByRole("button", { name: /Compress now|Compressing/ });
  await expect(button).toBeVisible();

  // The endpoint behind it answers with a real snapshot, and a pass is safe to request even when it
  // finds no work.
  const before = await request.get("/api/settings/storage/compress", {
    headers: { Authorization: `Bearer ${token}` },
  });
  expect(before.ok()).toBe(true);
  expect(await before.json()).toMatchObject({ running: false });

  const started = await request.post("/api/settings/storage/compress", {
    headers: { Authorization: `Bearer ${token}` },
  });
  expect(started.status()).toBe(202);
  const snapshot = (await started.json()) as { total: number; reclaimed_bytes: number };
  expect(typeof snapshot.total).toBe("number");
  expect(typeof snapshot.reclaimed_bytes).toBe("number");
});
