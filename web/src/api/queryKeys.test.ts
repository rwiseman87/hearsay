import { describe, expect, it } from "vitest";

import { queryKeys } from "./queryKeys";

describe("queryKeys factory", () => {
  it("shapes the meeting keys and shares one prefix for invalidation", () => {
    expect(queryKeys.meetings.all).toEqual(["meetings"]);
    expect(queryKeys.meetings.list(2, 50)).toEqual(["meetings", "list", 2, 50]);
    expect(queryKeys.meetings.segments("m1")).toEqual(["meetings", "segments", "m1"]);
    expect(queryKeys.meetings.speakers("m1")).toEqual(["meetings", "speakers", "m1"]);
    expect(queryKeys.meetings.notes("m1")).toEqual(["meetings", "notes", "m1"]);
    expect(queryKeys.meetings.userNotes("m1")).toEqual(["meetings", "userNotes", "m1"]);

    // Every meeting sub-key starts with the shared prefix, so invalidating `meetings.all` covers them.
    for (const key of [
      queryKeys.meetings.list(1, 10),
      queryKeys.meetings.segments("m1"),
      queryKeys.meetings.speakers("m1"),
      queryKeys.meetings.notes("m1"),
      queryKeys.meetings.userNotes("m1"),
    ]) {
      expect(key[0]).toBe("meetings");
    }
  });

  it("keys the other domains distinctly", () => {
    expect(queryKeys.folders.list(1, 200)).toEqual(["folders", "list", 1, 200]);
    expect(queryKeys.models.download).toEqual(["models", "download"]);
    expect(queryKeys.identities.list(1, 50)).toEqual(["identities", "list", 1, 50]);
    expect(queryKeys.settings.permissions).toEqual(["settings", "permissions"]);
    expect(queryKeys.status.all).toEqual(["status"]);
    // The search key preserves the (already-trimmed) query verbatim.
    expect(queryKeys.search.query("hi there")).toEqual(["search", "hi there"]);
  });
});
