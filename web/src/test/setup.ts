import { afterAll, afterEach, beforeAll } from "vitest";

import { server } from "./server";

// DOM shims for the jsdom-environment tests only (the pure client-layer tests opt into the node env,
// where these globals do not exist).
if (typeof window !== "undefined") {
  // Node 25 ships an experimental global `localStorage` that needs `--localstorage-file` to work and
  // otherwise shadows jsdom's with a non-functional stub. Replace it with a simple in-memory store so
  // components that persist UI state (e.g. the transcript recap width) run under test.
  class MemoryStorage {
    private store = new Map<string, string>();
    get length() {
      return this.store.size;
    }
    clear() {
      this.store.clear();
    }
    getItem(key: string) {
      return this.store.has(key) ? this.store.get(key)! : null;
    }
    key(index: number) {
      return [...this.store.keys()][index] ?? null;
    }
    removeItem(key: string) {
      this.store.delete(key);
    }
    setItem(key: string, value: string) {
      this.store.set(key, String(value));
    }
  }
  Object.defineProperty(globalThis, "localStorage", {
    value: new MemoryStorage(),
    configurable: true,
    writable: true,
  });

  // jsdom does not implement layout, so scrollIntoView is absent; several views call it in effects.
  Element.prototype.scrollIntoView = () => {};
}

// Global MSW lifecycle for the whole suite. `onUnhandledRequest: "error"` fails any test that makes a
// request with no registered handler; the client-layer tests avoid this by stubbing `fetch` or mocking
// modules, so anything that reaches MSW is a component test that forgot a handler. Handlers registered
// per-test are reset between tests so cases stay isolated.
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());
