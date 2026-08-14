-- The writable user-settings overlay. Each row stores one settings *section* (e.g. "recording")
-- as a JSON object string; the settings service resolves the effective value as the stored
-- override when present, otherwise the env/config default. `value` is a JSON object serialized
-- to TEXT, which is how SQLite stores a JSON column.

CREATE TABLE preferences (
    id         BLOB NOT NULL PRIMARY KEY,
    section    TEXT NOT NULL UNIQUE,
    value      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
