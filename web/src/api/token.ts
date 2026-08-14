// The core injects the per-session token into the served index.html as a global.
// In dev (Vite dev server) it is absent, so fall back to the ?token= query param
// that `make rust-serve` prints.
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
      "No session token. Open the URL printed by `make rust-serve` (it includes ?token=).",
    );
  }
  cached = token;
  // Deliberately leave ?token= in the URL on loopback. The core gates every request on the token,
  // including `GET /` (routes/web.rs), and the in-memory copy above does not survive a full document
  // reload. So a manual Cmd-R or the ErrorBoundary's window.location.reload() crash recovery must be
  // able to re-request `/` with the token still in the URL, or it 401s to a blank window. The project
  // forbids localStorage/sessionStorage for the token, so the loopback URL is the intended carrier.
  return token;
}
