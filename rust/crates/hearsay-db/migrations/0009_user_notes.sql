-- User-authored live notes: the free-form text a user types in the "My notes" panel during a
-- meeting, distinct from the LLM-generated `meeting_notes` (summary + action items). One row per
-- meeting, upserted on autosave. Cascades on meeting delete like segments/clusters/meeting_notes, so
-- delete-meeting stays a single DELETE. Exported to `my-notes.md` as a one-way copy (the DB is the
-- source of truth).

CREATE TABLE user_notes (
    meeting_id BLOB NOT NULL PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
