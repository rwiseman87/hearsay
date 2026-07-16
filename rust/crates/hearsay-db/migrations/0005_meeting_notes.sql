-- Post-meeting notes: one summary + action-item list per meeting, produced by the optional local
-- LLM summarization step (llama.cpp). One row per meeting (regenerate = upsert); action_items is a
-- JSON array of strings; model records which GGUF produced it. Cascades on meeting delete like
-- segments/clusters (so delete-meeting stays a single DELETE).

CREATE TABLE meeting_notes (
    meeting_id   BLOB NOT NULL PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    summary      TEXT NOT NULL,
    action_items TEXT NOT NULL,
    model        TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
