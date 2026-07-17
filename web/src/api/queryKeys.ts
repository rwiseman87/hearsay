// Typed query-key factory. All keys are produced here, never inlined at call
// sites; the "meetings" prefix lets a single invalidation cover list + segments.
export const queryKeys = {
  meetings: {
    all: ["meetings"] as const,
    list: (page: number, pageSize: number) => ["meetings", "list", page, pageSize] as const,
    // All segment pages are fetched under one query (see useSegments), so the key is per-meeting.
    segments: (id: string) => ["meetings", "segments", id] as const,
    speakers: (id: string) => ["meetings", "speakers", id] as const,
    notes: (id: string) => ["meetings", "notes", id] as const,
  },
  // Organizational folder tree for the sidebar. One prefix so a folder mutation invalidates the
  // whole set (the tree is rebuilt client-side from the flat list).
  folders: {
    all: ["folders"] as const,
    list: (page: number, pageSize: number) => ["folders", "list", page, pageSize] as const,
  },
  // Notes-model download manager (catalog + the single active download's progress).
  models: {
    catalog: ["models", "catalog"] as const,
    download: ["models", "download"] as const,
  },
  identities: {
    all: ["identities"] as const,
    list: (page: number, pageSize: number) => ["identities", "list", page, pageSize] as const,
  },
  // Full-text transcript search, keyed by the (trimmed) query string.
  search: {
    query: (q: string) => ["search", q] as const,
  },
  settings: {
    all: ["settings"] as const,
    permissions: ["settings", "permissions"] as const,
  },
  status: {
    all: ["status"] as const,
  },
} as const;
