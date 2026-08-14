-- User-facing meeting folders (a nested tree). Distinct from the existing `meetings.folder` column,
-- which is the on-disk recordings-directory name, not an organizational group. `parent_id` is a
-- self-reference (NULL = a root folder); deleting a folder removes its sub-folder subtree
-- (ON DELETE CASCADE) but un-files the meetings inside it rather than deleting them
-- (`meetings.folder_id ... ON DELETE SET NULL`). Both cascades fire recursively because the pool
-- sets PRAGMA foreign_keys=ON (see src/lib.rs). UUIDs are BLOB and timestamps TEXT (RFC3339), as
-- elsewhere. ADD COLUMN with a REFERENCES clause is permitted here because the default is NULL.

CREATE TABLE folders (
    id         BLOB NOT NULL PRIMARY KEY,
    name       TEXT NOT NULL,
    parent_id  BLOB REFERENCES folders(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX ix_folders_parent ON folders (parent_id);

ALTER TABLE meetings ADD COLUMN folder_id BLOB REFERENCES folders(id) ON DELETE SET NULL;

CREATE INDEX ix_meetings_folder ON meetings (folder_id);
