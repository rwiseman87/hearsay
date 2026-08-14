-- Baseline schema: one forward-only baseline for the whole data model. UUIDs are stored as BLOB
-- and timestamps as TEXT (RFC3339), matching the sqlx SQLite encoders; booleans as INTEGER.

CREATE TABLE meetings (
    id         BLOB NOT NULL PRIMARY KEY,
    title      TEXT NOT NULL,
    folder     TEXT NOT NULL,
    status     TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at   TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE identities (
    id           BLOB NOT NULL PRIMARY KEY,
    display_name TEXT NOT NULL UNIQUE,
    email        TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE TABLE clusters (
    id          BLOB    NOT NULL PRIMARY KEY,
    meeting_id  BLOB    NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    identity_id BLOB    REFERENCES identities(id) ON DELETE SET NULL,
    locked      INTEGER NOT NULL,
    centroid    BLOB,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL,
    UNIQUE (meeting_id, ordinal)
);

CREATE TABLE segments (
    id            BLOB NOT NULL PRIMARY KEY,
    meeting_id    BLOB NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    cluster_id    BLOB REFERENCES clusters(id) ON DELETE SET NULL,
    stream        TEXT NOT NULL,
    speaker_label TEXT NOT NULL,
    text          TEXT NOT NULL,
    start_s       REAL NOT NULL,
    end_s         REAL NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE INDEX ix_segments_meeting_start ON segments (meeting_id, start_s);
