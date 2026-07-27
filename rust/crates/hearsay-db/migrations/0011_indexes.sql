-- Indexes for columns the query layer filters or orders on that the baseline schema left unindexed.
-- Low impact on a single-user local DB today, but free and forward-proofing as histories grow.
--   segments.cluster_id   -- rename_cluster's UPDATE ... WHERE cluster_id, carry_forward's filter
--   clusters.identity_id  -- reassign_segment_speaker's WHERE identity_id, the known_voiceprints join
--   meetings.status       -- list_nonterminal_meetings (the startup reconcile sweep)
--   meetings.started_at   -- list_meetings ORDER BY started_at DESC

CREATE INDEX ix_segments_cluster ON segments (cluster_id);
CREATE INDEX ix_clusters_identity ON clusters (identity_id);
CREATE INDEX ix_meetings_status ON meetings (status);
CREATE INDEX ix_meetings_started_at ON meetings (started_at);
