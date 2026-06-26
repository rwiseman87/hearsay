// The core injects the per-session token into the served index.html as a global.
// In dev (Vite dev server) it is absent, so fall back to the ?token= query param
// that `hearsay serve` prints.
declare global {
  interface Window {
    __HEARSAY_TOKEN__?: string;
  }
}

let cached: string | null = null;

export function getToken(): string {
  if (cached) return cached;
  const fromQuery = new URLSearchParams(window.location.search).get("token");
  const token = window.__HEARSAY_TOKEN__ ?? fromQuery;
  if (!token) {
    throw new Error(
      "No session token. Open the URL printed by `hearsay serve` (it includes ?token=).",
    );
  }
  cached = token;
  return token;
}
