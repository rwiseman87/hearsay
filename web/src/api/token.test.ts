import { afterEach, describe, expect, it, vi } from "vitest";

// getToken caches in module scope, so reload the module per case to test each source independently.
async function loadGetToken() {
  vi.resetModules();
  return (await import("./token")).getToken;
}

describe("getToken", () => {
  afterEach(() => {
    delete window.__HEARSAY_TOKEN__;
    window.history.replaceState({}, "", "/");
  });

  it("prefers the token the core injects as a global", async () => {
    window.__HEARSAY_TOKEN__ = "injected";
    window.history.replaceState({}, "", "/?token=from-query");

    const getToken = await loadGetToken();
    expect(getToken()).toBe("injected");
  });

  it("falls back to the ?token= query param and caches it across URL changes", async () => {
    window.history.replaceState({}, "", "/?token=from-query");

    const getToken = await loadGetToken();
    expect(getToken()).toBe("from-query");

    // Cached: a later reload/URL change (e.g. after crash-recovery) keeps the first value.
    window.history.replaceState({}, "", "/");
    expect(getToken()).toBe("from-query");
  });

  it("throws a helpful error when no token is present", async () => {
    const getToken = await loadGetToken();
    expect(() => getToken()).toThrow(/no session token/i);
  });
});
