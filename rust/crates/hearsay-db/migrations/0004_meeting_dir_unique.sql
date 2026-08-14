-- Data integrity: a per-meeting recordings directory must be unique. Folder names have minute
-- resolution, so two same-title meetings within one minute otherwise resolve to one shared `dir`
-- that a later delete would wipe out from under the first. The orchestrator resolves a
-- collision-suffixed directory at creation; this index is the backstop.
--
-- Partial index: legacy rows (migration 0003) store dir = '' as the "fall back to
-- output_dir/<folder>" sentinel and may share it, so a plain UNIQUE index would fail to apply.
-- Uniqueness therefore applies only to real, pinned directories.
CREATE UNIQUE INDEX ix_meetings_dir_unique ON meetings (dir) WHERE dir != '';
