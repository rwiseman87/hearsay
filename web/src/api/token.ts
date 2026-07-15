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
  // Strip ?token= from the URL once cached so the secret does not linger in the address bar or
  // browser history for the rest of the session (it is already held in memory).
  if (fromQuery) {
    stripTokenFromUrl();
  }
  return token;
}

function stripTokenFromUrl(): void {
  try {
    const url = new URL(window.location.href);
    if (url.searchParams.has("token")) {
      url.searchParams.delete("token");
      const search = url.searchParams.toString();
      const next = url.pathname + (search ? `?${search}` : "") + url.hash;
      window.history.replaceState(window.history.state, "", next);
    }
  } catch {
    // Non-fatal: keeping the token in the URL is a hygiene issue, not a functional one.
  }
}
