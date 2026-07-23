// Shared constants for the browser E2E (imported by both playwright.config.ts and the specs).
// Everything the run writes lands under the repo-root, already-gitignored outputs/e2e/.
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url)); // web/e2e
export const E2E_OUTPUT_DIR = path.resolve(here, "../../outputs/e2e");
// The core writes {port, token} here after it binds (the same private handshake file the desktop
// shell reads); the spec reads the session token from it rather than scraping stdout.
export const HANDSHAKE_PATH = path.join(E2E_OUTPUT_DIR, "handshake.json");

// A placeholder "notes model" file. The scripted summarizer ignores the model contents entirely, but
// the Settings > Models check (and thus the enabled state of the "Generate notes" button) requires
// the configured notes model path to be an existing file, so the config points HEARSAY_NOTES_MODEL
// here after creating it.
export const NOTES_MODEL_STUB = path.join(E2E_OUTPUT_DIR, "notes-model.stub");

// The throwaway SQLite DB the scripted core runs against (migrations apply on boot). Wiped at the
// start of each run so a fixed meeting title stays unambiguous in the Library.
export const DB_PATH = path.join(E2E_OUTPUT_DIR, "e2e.db");

// The core runs on the fixed dev port the vite proxy (vite.config.ts) already targets; vite serves
// the app the browser drives.
export const CORE_PORT = 8137;
export const VITE_PORT = 5173;
