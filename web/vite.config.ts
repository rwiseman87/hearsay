import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

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
});
