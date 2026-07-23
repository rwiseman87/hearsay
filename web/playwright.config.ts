import fs from "node:fs";
import path from "node:path";

import { defineConfig } from "@playwright/test";

import {
  CORE_PORT,
  DB_PATH,
  E2E_OUTPUT_DIR,
  HANDSHAKE_PATH,
  NOTES_MODEL_STUB,
  VITE_PORT,
} from "./e2e/paths";

// Runs when Playwright loads this config, before it starts the webServers, so the stub notes-model
// file exists by the time the core boots and resolves HEARSAY_NOTES_MODEL (which gates the UI's
// "Generate notes" button on the file existing). Kept idempotent and non-destructive on purpose:
// Playwright re-imports this config in each worker (after the core has already written its handshake),
// so deleting the handshake/DB here would race the running core. The core overwrites the handshake on
// boot, and the spec uses a per-run meeting title, so no wipe is needed.
fs.mkdirSync(E2E_OUTPUT_DIR, { recursive: true });
fs.writeFileSync(NOTES_MODEL_STUB, "");

// Windows must compile the sherpa backend into the core (a compile_error otherwise); the scripted
// engine itself is platform-neutral, so macOS needs no features. scripts\test-windows.ps1 sets this.
const coreFeatures = process.env.HEARSAY_CORE_FEATURES;
const coreRun = [
  "cargo run -q --manifest-path ../rust/Cargo.toml -p hearsay-core",
  coreFeatures ? `--features ${coreFeatures}` : "",
]
  .filter(Boolean)
  .join(" ");

// The scripted core boots headless with a canned, model-free meeting (HEARSAY_SCRIPTED, honored only
// in development) writing its DB + outputs + handshake under the gitignored outputs/e2e/.
const coreEnv: Record<string, string> = {
  HEARSAY_SCRIPTED: "1",
  ENVIRONMENT: "development",
  HEARSAY_SERVER_HOST: "127.0.0.1",
  HEARSAY_SERVER_PORT: String(CORE_PORT),
  DATABASE_URL: `sqlite://${DB_PATH}`,
  HEARSAY_OUTPUT_DIR: path.join(E2E_OUTPUT_DIR, "out"),
  HEARSAY_HANDSHAKE_PATH: HANDSHAKE_PATH,
  // Makes ModelsInfo.notes_model_exists true so the "Generate notes" button enables; the scripted
  // summarizer ignores the file, so a stub suffices.
  HEARSAY_NOTES_MODEL: NOTES_MODEL_STUB,
};

export default defineConfig({
  testDir: "./e2e",
  outputDir: path.join(E2E_OUTPUT_DIR, "test-results"),
  // Deterministic + serial: one scripted core, one meeting per spec.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  reporter: [
    ["list"],
    ["html", { outputFolder: path.join(E2E_OUTPUT_DIR, "playwright-report"), open: "never" }],
  ],
  use: {
    baseURL: `http://localhost:${VITE_PORT}`,
    // Triage artifacts on failure only (per docs/testing.md "Results & cleanup").
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  // Playwright starts (and stops) both servers around the run. `port` waits for a TCP accept — the
  // core gates every HTTP route on the token, so an HTTP-status readiness probe would 401; a TCP
  // probe sidesteps that. The core is never reused (it must be the fresh scripted instance); reusing
  // an already-running vite dev server is harmless.
  webServer: [
    {
      command: coreRun,
      port: CORE_PORT,
      env: coreEnv,
      reuseExistingServer: false,
      stdout: "pipe",
      stderr: "pipe",
      timeout: 120_000,
    },
    {
      command: `npm run dev -- --port ${VITE_PORT} --strictPort`,
      port: VITE_PORT,
      reuseExistingServer: true,
      timeout: 60_000,
    },
  ],
});
