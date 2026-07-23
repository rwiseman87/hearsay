import { afterAll, afterEach, beforeAll } from "vitest";

import { server } from "./server";

// Global MSW lifecycle for the whole suite. `onUnhandledRequest: "error"` fails any test that makes a
// request with no registered handler; the client-layer tests avoid this by stubbing `fetch` or mocking
// modules, so anything that reaches MSW is a component test that forgot a handler. Handlers registered
// per-test are reset between tests so cases stay isolated.
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());
