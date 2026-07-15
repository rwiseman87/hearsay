// Typed query-key factory. All keys are produced here, never inlined at call
// sites; the "meetings" prefix lets a single invalidation cover list + segments.
export const queryKeys = {
  meetings: {
    all: ["meetings"] as const,
    list: (page: number, pageSize: number) => ["meetings", "list", page, pageSize] as const,
    // All segment pages are fetched under one query (see useSegments), so the key is per-meeting.
    segments: (id: string) => ["meetings", "segments", id] as const,
    speakers: (id: string) => ["meetings", "speakers", id] as const,
  },
  identities: {
    all: ["identities"] as const,
    list: (page: number, pageSize: number) => ["identities", "list", page, pageSize] as const,
  },
  settings: {
    all: ["settings"] as const,
    permissions: ["settings", "permissions"] as const,
  },
  status: {
    all: ["status"] as const,
  },
} as const;
