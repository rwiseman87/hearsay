import { setupServer } from "msw/node";

// Shared MSW server for the component tests, which register per-test handlers with `server.use(...)`.
// The client-layer unit tests stub `fetch` or mock the api module directly and never reach it. The
// listen/reset/close lifecycle lives in ./setup.ts.
export const server = setupServer();
