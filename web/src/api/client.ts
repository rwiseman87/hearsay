// The single canonical fetch wrapper: carries the per-session bearer token,
// normalizes the `{ detail }` error envelope into ApiError, and applies a
// default timeout via AbortSignal. Feature code never calls fetch directly.
import { getToken } from "./token";

export class ApiError extends Error {
  readonly status: number;
  // The raw `detail` from the error envelope. Usually a string, but some endpoints return a
  // structured object (e.g. relocation returns `{ message, missing }`) the UI renders richly.
  readonly detail: unknown;

  constructor(status: number, message: string, detail?: unknown) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.detail = detail;
  }
}

const DEFAULT_TIMEOUT_MS = 15_000;

interface RequestOptions {
  method?: string;
  body?: unknown;
  signal?: AbortSignal;
  timeoutMs?: number;
}

function describeError(data: unknown, fallback: string): string {
  if (data && typeof data === "object" && "detail" in data) {
    const detail = (data as { detail: unknown }).detail;
    return typeof detail === "string" ? detail : JSON.stringify(detail);
  }
  return fallback;
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = "GET", body, signal, timeoutMs = DEFAULT_TIMEOUT_MS } = options;
  const timeout = AbortSignal.timeout(timeoutMs);
  const composed = signal ? AbortSignal.any([signal, timeout]) : timeout;

  const headers: Record<string, string> = { Authorization: `Bearer ${getToken()}` };
  if (body !== undefined) headers["Content-Type"] = "application/json";

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: composed,
    });
  } catch (cause) {
    throw new ApiError(0, cause instanceof Error ? cause.message : "network error");
  }

  if (response.status === 204) return undefined as T;

  const text = await response.text();
  const data: unknown = text ? JSON.parse(text) : undefined;
  if (!response.ok) {
    const detail =
      data && typeof data === "object" && "detail" in data
        ? (data as { detail: unknown }).detail
        : undefined;
    throw new ApiError(response.status, describeError(data, response.statusText), detail);
  }
  return data as T;
}

export const api = {
  get: <T>(path: string, signal?: AbortSignal): Promise<T> => request<T>(path, { signal }),
  post: <T>(path: string, body?: unknown, opts?: { timeoutMs?: number }): Promise<T> =>
    request<T>(path, { method: "POST", body, timeoutMs: opts?.timeoutMs }),
  put: <T>(path: string, body?: unknown): Promise<T> => request<T>(path, { method: "PUT", body }),
  patch: <T>(path: string, body?: unknown): Promise<T> =>
    request<T>(path, { method: "PATCH", body }),
  delete: <T>(path: string): Promise<T> => request<T>(path, { method: "DELETE" }),
};
