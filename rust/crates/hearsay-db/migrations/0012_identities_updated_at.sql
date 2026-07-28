-- list_identities orders known people by updated_at DESC (rename suggestions); 0011_indexes.sql
-- covered meetings/segments/clusters but overlooked this one. Small table today, but free.
CREATE INDEX ix_identities_updated_at ON identities (updated_at);
