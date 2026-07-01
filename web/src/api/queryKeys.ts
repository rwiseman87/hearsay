// Typed query-key factory. All keys are produced here, never inlined at call
// sites; the "meetings" prefix lets a single invalidation cover list + segments.
export const queryKeys = {
  meetings: {
    all: ["meetings"] as const,
    list: (page: number, pageSize: number) => ["meetings", "list", page, pageSize] as const,
    segments: (id: string, page: number, pageSize: number) =>
      ["meetings", "segments", id, page, pageSize] as const,
    speakers: (id: string) => ["meetings", "speakers", id] as const,
  },
  identities: {
    all: ["identities"] as const,
    list: (page: number, pageSize: number) => ["identities", "list", page, pageSize] as const,
  },
} as const;
