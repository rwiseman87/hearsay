import { MutationCache, QueryCache, QueryClient } from "@tanstack/react-query";

import { ApiError } from "./client";

function report(error: unknown): void {
  console.error("[hearsay] request failed:", error);
}

// Client-level defaults + error handlers (per-view code can still read query
// error state for inline messages).
export const queryClient = new QueryClient({
  queryCache: new QueryCache({ onError: report }),
  mutationCache: new MutationCache({ onError: report }),
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      refetchOnWindowFocus: false,
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
          return false;
        }
        return failureCount < 2;
      },
    },
  },
});
