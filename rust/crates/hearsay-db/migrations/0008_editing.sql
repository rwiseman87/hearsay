-- In-app editing of transcripts and notes. Each row gains an `edited` flag, set when a user manually
-- edits it, so the UI can badge edits and warn before a destructive refine / notes-regenerate would
-- overwrite them. The DB stays the source of truth (the .md files are one-way exports).
ALTER TABLE segments ADD COLUMN edited INTEGER NOT NULL DEFAULT 0;
ALTER TABLE meeting_notes ADD COLUMN edited INTEGER NOT NULL DEFAULT 0;
