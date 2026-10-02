-- Audible stretches of at least 10 s with no transcript at the last refine, as JSON [[start_s, end_s], ...].
-- NULL = never refined, or refined before this column existed.
ALTER TABLE meetings ADD COLUMN refine_gaps TEXT;
