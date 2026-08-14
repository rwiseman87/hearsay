-- Per-meeting recordings directory. `folder` is only the leaf name; historically its root was the
-- process-wide output_dir. Once the Storage settings panel can change that root at runtime, the root
-- must be pinned per meeting so existing recordings stay locatable after the setting changes. New
-- meetings store their absolute directory here; legacy rows keep '' and fall back to
-- output_dir.join(folder) (see Meeting::dir_path).
ALTER TABLE meetings ADD COLUMN dir TEXT NOT NULL DEFAULT '';
