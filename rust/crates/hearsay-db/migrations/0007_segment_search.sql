-- Full-text search over transcript segments (SQLite FTS5). An external-content index mirroring
-- `segments.text`, so the transcript is stored once and the index is kept in sync by triggers. This
-- keeps the destructive refine (replace_them_segments bulk delete+insert), manual segment edits, and
-- the delete-meeting cascade correct with no changes to the query layer. `segments` has an implicit
-- integer rowid (its primary key is a BLOB UUID), which the external-content table indexes.

CREATE VIRTUAL TABLE segments_fts USING fts5(
    text,
    content='segments',
    content_rowid='rowid',
    tokenize='unicode61'
);

-- Backfill the index from the existing rows (a no-op on a fresh install).
INSERT INTO segments_fts(segments_fts) VALUES('rebuild');

-- Keep the index in lockstep with the content table. The 'delete' command is the documented way to
-- remove an external-content row's old terms before re-inserting on update.
CREATE TRIGGER segments_ai AFTER INSERT ON segments BEGIN
    INSERT INTO segments_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER segments_ad AFTER DELETE ON segments BEGIN
    INSERT INTO segments_fts(segments_fts, rowid, text) VALUES('delete', old.rowid, old.text);
END;
CREATE TRIGGER segments_au AFTER UPDATE ON segments BEGIN
    INSERT INTO segments_fts(segments_fts, rowid, text) VALUES('delete', old.rowid, old.text);
    INSERT INTO segments_fts(rowid, text) VALUES (new.rowid, new.text);
END;
