// @vitest-environment node
import { describe, expect, it, vi } from "vitest";

// The wrapper reads the session token via getToken(), which touches `window`; the node env has none,
// so stub it. The token-plumbing itself is covered in token.test.ts (jsdom).
vi.mock("./token", () => ({ getToken: () => "test-token" }));

import { api, ApiError } from "./client";

type FetchArgs = [string, RequestInit];

function jsonResponse(status: number, body: unknown, statusText?: string): Response {
  return new Response(JSON.stringify(body), {
    status,
    statusText,
    headers: { "Content-Type": "application/json" },
  });
}

function stubFetch(make: () => Promise<Response>) {
  const fn = vi.fn(make);
  vi.stubGlobal("fetch", fn);
  return fn;
}

function headersOf(call: FetchArgs): Record<string, string> {
  return call[1].headers as Record<string, string>;
}

describe("api client", () => {
  it("GET carries the bearer token and a request id, with no body or content-type", async () => {
    const fetchMock = stubFetch(async () => jsonResponse(200, { ok: true }));

    const result = await api.get<{ ok: boolean }>("/api/thing");

    expect(result).toEqual({ ok: true });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const call = fetchMock.mock.calls[0] as unknown as FetchArgs;
    expect(call[0]).toBe("/api/thing");
    expect(call[1].method).toBe("GET");
    expect(call[1].body).toBeUndefined();
    const headers = headersOf(call);
    expect(headers.Authorization).toBe("Bearer test-token");
    expect(headers["X-Request-Id"]).toMatch(/^[0-9a-f-]{36}$/i);
    expect(headers["Content-Type"]).toBeUndefined();
  });

  it("POST serializes the body as JSON and sets Content-Type", async () => {
    const fetchMock = stubFetch(async () => jsonResponse(200, { id: "m1" }));

    await api.post("/api/meetings", { title: "Sync" });

    const call = fetchMock.mock.calls[0] as unknown as FetchArgs;
    expect(call[1].method).toBe("POST");
    expect(headersOf(call)["Content-Type"]).toBe("application/json");
    expect(call[1].body).toBe(JSON.stringify({ title: "Sync" }));
  });

  it("sends no body for a POST without one (204 keep-recording style)", async () => {
    const fetchMock = stubFetch(async () => new Response(null, { status: 204 }));

    const result = await api.post<void>("/api/meetings/1/keep-recording");

    expect(result).toBeUndefined();
    const call = fetchMock.mock.calls[0] as unknown as FetchArgs;
    expect(call[1].body).toBeUndefined();
    expect(headersOf(call)["Content-Type"]).toBeUndefined();
  });

  it("returns undefined for 204 No Content without parsing a body", async () => {
    stubFetch(async () => new Response(null, { status: 204 }));

    await expect(api.delete<void>("/api/meetings/1")).resolves.toBeUndefined();
  });

  it("normalizes a string `detail` error envelope into ApiError", async () => {
    stubFetch(async () => jsonResponse(422, { detail: "title too long" }));

    const err = await api.post("/api/meetings", {}).catch((e: unknown) => e);

    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 422, message: "title too long", detail: "title too long" });
  });

  it("stringifies a structured `detail` for the message but preserves the raw object", async () => {
    const detail = { message: "missing files", missing: ["audio.wav"] };
    stubFetch(async () => jsonResponse(409, { detail }));

    const err = (await api.post("/api/meetings/1/relocate", {}).catch((e: unknown) => e)) as ApiError;

    expect(err.message).toBe(JSON.stringify(detail));
    expect(err.detail).toEqual(detail);
  });

  it("falls back to the status text when the error body has no detail", async () => {
    stubFetch(async () => jsonResponse(500, { error: "boom" }, "Internal Server Error"));

    const err = (await api.get("/api/thing").catch((e: unknown) => e)) as ApiError;

    expect(err.status).toBe(500);
    expect(err.message).toBe("Internal Server Error");
    expect(err.detail).toBeUndefined();
  });

  it("wraps a fetch/network failure as ApiError(0)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("Failed to fetch");
      }),
    );

    const err = (await api.get("/api/thing").catch((e: unknown) => e)) as ApiError;

    expect(err).toBeInstanceOf(ApiError);
    expect(err.status).toBe(0);
    expect(err.message).toBe("Failed to fetch");
  });

  it("composes the caller's AbortSignal with the request timeout", async () => {
    const controller = new AbortController();
    let seen: AbortSignal | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_path: string, init: RequestInit) => {
        seen = init.signal ?? undefined;
        return jsonResponse(200, {});
      }),
    );

    await api.get("/api/thing", controller.signal);

    expect(seen).toBeDefined();
    expect(seen?.aborted).toBe(false);
    controller.abort();
    expect(seen?.aborted).toBe(true);
  });
});
