import type { MeetingSort } from "./types";

// Everything that scopes a meetings listing; two calls with the same values share a cache entry.
export interface MeetingListKey {
  page: number;
  pageSize: number;
  folderId: string | null;
  unfiled: boolean;
  q: string;
  sort: MeetingSort;
}

// Typed query-key factory. All keys are produced here, never inlined at call
// sites; the "meetings" prefix lets a single invalidation cover list + segments.
export const queryKeys = {
  meetings: {
    all: ["meetings"] as const,
    // The list is filtered server-side, so the whole filter (not just the page) identifies it.
    list: (params: MeetingListKey) => ["meetings", "list", params] as const,
    // Per-folder counts for the sidebar badges: one row set, independent of any listed page.
    counts: ["meetings", "counts"] as const,
    detail: (id: string) => ["meetings", "detail", id] as const,
    // All segment pages are fetched under one query (see useSegments), so the key is per-meeting.
    segments: (id: string) => ["meetings", "segments", id] as const,
    speakers: (id: string) => ["meetings", "speakers", id] as const,
    notes: (id: string) => ["meetings", "notes", id] as const,
    userNotes: (id: string) => ["meetings", "userNotes", id] as const,
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
  // The stored-voiceprint roster (people + the per-meeting voice samples recognition draws on).
  voiceprints: {
    all: ["voiceprints"] as const,
    list: (page: number, pageSize: number) => ["voiceprints", "list", page, pageSize] as const,
  },
  // Full-text transcript search, keyed by the (trimmed) query string.
  search: {
    query: (q: string) => ["search", q] as const,
  },
  settings: {
    all: ["settings"] as const,
    permissions: ["settings", "permissions"] as const,
    archive: ["settings", "archive"] as const,
  },
  status: {
    all: ["status"] as const,
  },
  // First-run model setup (whether models are still missing, and a run's progress).
  setup: {
    all: ["setup"] as const,
  },
} as const;
