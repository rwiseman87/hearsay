import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Dev only. By default the core auto-picks a free port; for the Vite dev
// proxy run it on a fixed port instead: `RUST_PORT=8137 make rust-serve`.
// In production the bundle is served same-origin by the core, so these proxies
// are unused.
const CORE_URL = "http://127.0.0.1:8137";

export default defineConfig({
  plugins: [react()],
  build: { outDir: "dist" },
  server: {
    proxy: {
      "/api": { target: CORE_URL, changeOrigin: true },
      "/ws": { target: CORE_URL, changeOrigin: true, ws: true },
    },
  },
  // Unit/component tests (vitest). jsdom for the React/DOM tests; the pure client-layer
  // tests opt into the node environment per-file (`// @vitest-environment node`). Globals so
  // Testing Library's auto-cleanup runs; stubbed globals/mocks reset between tests. Coverage lands
  // under the repo-root, already-gitignored outputs/ (see docs/testing.md, "Results & cleanup").
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    restoreMocks: true,
    unstubGlobals: true,
    coverage: {
      provider: "v8",
      reportsDirectory: "../outputs/coverage/web",
      reporter: ["text", "html", "lcov"],
      include: ["src/**/*.{ts,tsx}"],
      exclude: [
        "src/**/*.{test,spec}.{ts,tsx}",
        "src/test/**",
        "src/api/schema.ts",
        "src/main.tsx",
        "src/vite-env.d.ts",
      ],
    },
  },
});
