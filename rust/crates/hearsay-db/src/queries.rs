//! Typed queries over the schema, runtime-checked (`sqlx::query` / `query_as`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use hearsay_attribution::{
    assign_segment_speaker, best_identity, centroid_from_bytes, centroid_to_bytes, SpeakerTurn,
};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};
use uuid::Uuid;

/// SQL for the known cross-meeting voiceprints: every person named + locked in another meeting with
/// a stored centroid.
const KNOWN_VOICEPRINTS_SQL: &str = "SELECT i.display_name, c.centroid FROM clusters c \
     JOIN identities i ON i.id = c.identity_id \
     WHERE c.locked = 1 AND c.centroid IS NOT NULL AND c.meeting_id != ?";

use crate::models::{
    Cluster, Folder, Identity, Meeting, MeetingNotes, MeetingStatus, Segment, Stream, UserNotes,
};

/// A speaker cluster joined to its bound identity's name (for the speakers list). `display_name`
/// is `None` when the cluster is unbound; the caller renders `"Speaker {ordinal}"` in that case.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct SpeakerRow {
    pub id: Uuid,
    pub ordinal: i64,
    pub identity_id: Option<Uuid>,
    pub locked: bool,
    pub display_name: Option<String>,
}

/// How many meetings sit in one folder, or (with a `None` id) in no folder at all.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct FolderMeetingCount {
    pub folder_id: Option<Uuid>,
    pub meetings: i64,
}

/// Create a `recording` meeting (with its recordings `dir` pinned in the same statement) and return
/// the inserted row. Pass `""` for `dir` to rely on the `Meeting::dir_path` fallback
/// (`output_dir.join(folder)`), as legacy rows do.
pub async fn create_meeting(
    pool: &SqlitePool,
    title: &str,
    folder: &str,
    dir: &str,
    started_at: DateTime<Utc>,
) -> Result<Meeting, sqlx::Error> {
    let now = Utc::now();
    let meeting = Meeting {
        id: Uuid::new_v4(),
        title: title.to_string(),
        folder: folder.to_string(),
        status: MeetingStatus::Recording,
        started_at,
        ended_at: None,
        created_at: now,
        updated_at: now,
        dir: dir.to_string(),
        folder_id: None,
        refine_coverage: None,
    };
    sqlx::query(
        "INSERT INTO meetings \
         (id, title, folder, status, started_at, ended_at, created_at, updated_at, dir) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(meeting.id)
    .bind(&meeting.title)
    .bind(&meeting.folder)
    .bind(meeting.status)
    .bind(meeting.started_at)
    .bind(meeting.ended_at)
    .bind(meeting.created_at)
    .bind(meeting.updated_at)
    .bind(&meeting.dir)
    .execute(pool)
    .await?;
    Ok(meeting)
}

/// Pin a meeting's absolute recordings directory (set once at creation, after the folder is made).
/// Stored so the meeting stays locatable if the Storage output_dir setting later changes.
pub async fn set_meeting_dir(
    pool: &SqlitePool,
    meeting_id: Uuid,
    dir: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE meetings SET dir = ?, updated_at = ? WHERE id = ?")
        .bind(dir)
        .bind(Utc::now())
        .bind(meeting_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Fetch a meeting by id, or `None` if it does not exist.
pub async fn get_meeting(pool: &SqlitePool, id: Uuid) -> Result<Option<Meeting>, sqlx::Error> {
    sqlx::query_as::<_, Meeting>("SELECT * FROM meetings WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Rename a meeting (replace its display `title`), stamping `updated_at`. Returns the updated row,
/// or `None` when no meeting has that id. The caller validates `title` (non-empty, length bound).
pub async fn update_meeting_title(
    pool: &SqlitePool,
    id: Uuid,
    title: &str,
) -> Result<Option<Meeting>, sqlx::Error> {
    let result = sqlx::query("UPDATE meetings SET title = ?, updated_at = ? WHERE id = ?")
        .bind(title)
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_meeting(pool, id).await
}

/// Stamp a meeting's end time and set its post-stop `status` (`refining` while the background
/// refine + transcript write run, else `finalized`).
pub async fn finalize_meeting(
    pool: &SqlitePool,
    id: Uuid,
    ended_at: DateTime<Utc>,
    status: MeetingStatus,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE meetings SET status = ?, ended_at = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(ended_at)
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Flip a meeting from `refining` to `finalized` once the post-stop refine + transcript write
/// complete. Leaves `ended_at` (stamped at stop) untouched.
pub async fn set_meeting_finalized(pool: &SqlitePool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE meetings SET status = ?, updated_at = ? WHERE id = ?")
        .bind(MeetingStatus::Finalized)
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Meetings still in a non-terminal state (`recording` or `refining`). At startup no meeting can be
/// active, so every such row was stranded by a prior hard exit (SIGKILL / panic / power loss); the
/// reconcile sweep finalizes them. Ordered oldest-first for stable logging.
pub async fn list_nonterminal_meetings(pool: &SqlitePool) -> Result<Vec<Meeting>, sqlx::Error> {
    sqlx::query_as::<_, Meeting>(
        "SELECT * FROM meetings WHERE status IN (?, ?) ORDER BY started_at",
    )
    .bind(MeetingStatus::Recording)
    .bind(MeetingStatus::Refining)
    .fetch_all(pool)
    .await
}

/// Finalized meetings that ended before `cutoff`, oldest first — the archival sweep's work list.
///
/// `ended_at` is nullable (a row finalized by the startup reconcile after a hard exit may never have
/// stamped one), so the age falls back to `started_at`; without that, such a meeting would never
/// become eligible. Only `finalized` rows are returned, so a live or refining meeting is never a
/// candidate.
pub async fn list_finalized_before(
    pool: &SqlitePool,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Meeting>, sqlx::Error> {
    sqlx::query_as::<_, Meeting>(
        "SELECT * FROM meetings WHERE status = ? AND COALESCE(ended_at, started_at) < ? \
         ORDER BY started_at",
    )
    .bind(MeetingStatus::Finalized)
    .bind(cutoff)
    .fetch_all(pool)
    .await
}

/// Finalize a meeting stranded in a non-terminal state by a prior hard exit: set `finalized`, and
/// stamp `ended_at` only when it was never set (a row that died mid-`refining` already has it). Used
/// by the startup reconcile sweep, never during normal stop.
pub async fn reconcile_finalize_meeting(pool: &SqlitePool, id: Uuid) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    sqlx::query(
        "UPDATE meetings SET status = ?, ended_at = COALESCE(ended_at, ?), updated_at = ? WHERE id = ?",
    )
    .bind(MeetingStatus::Finalized)
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert a transcript segment and return the inserted row. `cluster_id` binds the segment to a
/// speaker cluster (Them finals); it is `None` for Me and for as-yet-unclustered rows.
#[allow(clippy::too_many_arguments)]
pub async fn insert_segment(
    pool: &SqlitePool,
    meeting_id: Uuid,
    stream: Stream,
    speaker_label: &str,
    text: &str,
    start_s: f64,
    end_s: f64,
    cluster_id: Option<Uuid>,
) -> Result<Segment, sqlx::Error> {
    let now = Utc::now();
    let segment = Segment {
        id: Uuid::new_v4(),
        meeting_id,
        cluster_id,
        stream,
        speaker_label: speaker_label.to_string(),
        text: text.to_string(),
        start_s,
        end_s,
        created_at: now,
        updated_at: now,
        edited: false,
    };
    sqlx::query(
        "INSERT INTO segments \
         (id, meeting_id, cluster_id, stream, speaker_label, text, start_s, end_s, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(segment.id)
    .bind(segment.meeting_id)
    .bind(segment.cluster_id)
    .bind(segment.stream)
    .bind(&segment.speaker_label)
    .bind(&segment.text)
    .bind(segment.start_s)
    .bind(segment.end_s)
    .bind(segment.created_at)
    .bind(segment.updated_at)
    .execute(pool)
    .await?;
    Ok(segment)
}

/// List a meeting's segments ordered by start time.
pub async fn list_segments(
    pool: &SqlitePool,
    meeting_id: Uuid,
) -> Result<Vec<Segment>, sqlx::Error> {
    sqlx::query_as::<_, Segment>("SELECT * FROM segments WHERE meeting_id = ? ORDER BY start_s")
        .bind(meeting_id)
        .fetch_all(pool)
        .await
}

/// Create an identity (unique `display_name`) and return the inserted row.
pub async fn create_identity(
    pool: &SqlitePool,
    display_name: &str,
    email: Option<&str>,
) -> Result<Identity, sqlx::Error> {
    let now = Utc::now();
    let identity = Identity {
        id: Uuid::new_v4(),
        display_name: display_name.to_string(),
        email: email.map(str::to_string),
        created_at: now,
        updated_at: now,
    };
    sqlx::query(
        "INSERT INTO identities (id, display_name, email, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(identity.id)
    .bind(&identity.display_name)
    .bind(&identity.email)
    .bind(identity.created_at)
    .bind(identity.updated_at)
    .execute(pool)
    .await?;
    Ok(identity)
}

/// Create a speaker cluster and return the inserted row.
pub async fn create_cluster(
    pool: &SqlitePool,
    meeting_id: Uuid,
    ordinal: i64,
    locked: bool,
    centroid: Option<Vec<u8>>,
) -> Result<Cluster, sqlx::Error> {
    let now = Utc::now();
    let cluster = Cluster {
        id: Uuid::new_v4(),
        meeting_id,
        ordinal,
        identity_id: None,
        locked,
        centroid,
        created_at: now,
        updated_at: now,
    };
    sqlx::query(
        "INSERT INTO clusters \
         (id, meeting_id, ordinal, identity_id, locked, centroid, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(cluster.id)
    .bind(cluster.meeting_id)
    .bind(cluster.ordinal)
    .bind(cluster.identity_id)
    .bind(cluster.locked)
    .bind(&cluster.centroid)
    .bind(cluster.created_at)
    .bind(cluster.updated_at)
    .execute(pool)
    .await?;
    Ok(cluster)
}

/// Which folder bucket a meetings listing covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FolderScope {
    #[default]
    All,
    Folder(Uuid),
    Unfiled,
}

/// A meetings listing: folder bucket, optional title match, sort direction. The Library's filters
/// run here rather than over a fetched page, so they cover every meeting however many there are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MeetingFilter {
    pub scope: FolderScope,
    /// Title substring, matched case-insensitively for ASCII (SQLite's default `LIKE`).
    pub title: Option<String>,
    pub oldest_first: bool,
}

impl MeetingFilter {
    /// Push this filter's `WHERE` clause. Only fixed SQL is written; user values are bound.
    fn push_where(&self, builder: &mut QueryBuilder<Sqlite>) {
        builder.push(" WHERE ");
        match self.scope {
            // No folder restriction; the constant keeps the title clause below unconditional.
            FolderScope::All => {
                builder.push("1 = 1");
            }
            FolderScope::Folder(id) => {
                builder.push("folder_id = ");
                builder.push_bind(id);
            }
            FolderScope::Unfiled => {
                builder.push("folder_id IS NULL");
            }
        }
        if let Some(title) = &self.title {
            builder.push(" AND title LIKE ");
            builder.push_bind(like_pattern(title));
            builder.push(" ESCAPE '\\'");
        }
    }
}

/// A `LIKE` substring pattern for `text`, with any wildcard the user typed escaped to a literal.
fn like_pattern(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Meeting count for `filter` (the `total` behind the paginated list envelope).
pub async fn count_meetings_filtered(
    pool: &SqlitePool,
    filter: &MeetingFilter,
) -> Result<i64, sqlx::Error> {
    let mut builder = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM meetings");
    filter.push_where(&mut builder);
    builder.build_query_scalar::<i64>().fetch_one(pool).await
}

/// Total meeting count, ignoring any filter.
pub async fn count_meetings(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    count_meetings_filtered(pool, &MeetingFilter::default()).await
}

/// One page of the meetings matching `filter`, by start time (newest first unless asked otherwise).
pub async fn list_meetings(
    pool: &SqlitePool,
    filter: &MeetingFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<Meeting>, sqlx::Error> {
    let mut builder = QueryBuilder::<Sqlite>::new("SELECT * FROM meetings");
    filter.push_where(&mut builder);
    builder.push(if filter.oldest_first {
        " ORDER BY started_at ASC LIMIT "
    } else {
        " ORDER BY started_at DESC LIMIT "
    });
    builder.push_bind(limit);
    builder.push(" OFFSET ");
    builder.push_bind(offset);
    builder.build_query_as::<Meeting>().fetch_all(pool).await
}

/// Meeting counts grouped by folder, with a `None` id for the unfiled bucket. One grouped scan
/// backs every sidebar badge, so the badges never depend on which page is on screen.
pub async fn count_meetings_by_folder(
    pool: &SqlitePool,
) -> Result<Vec<FolderMeetingCount>, sqlx::Error> {
    sqlx::query_as::<_, FolderMeetingCount>(
        "SELECT folder_id, COUNT(*) AS meetings FROM meetings GROUP BY folder_id",
    )
    .fetch_all(pool)
    .await
}

/// Delete a meeting (segments + clusters cascade). Returns whether a row was removed.
pub async fn delete_meeting(pool: &SqlitePool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM meetings WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Create a folder (optionally nested under `parent_id`) and return the inserted row. The caller
/// validates `name` and that `parent_id` (when given) exists.
pub async fn create_folder(
    pool: &SqlitePool,
    name: &str,
    parent_id: Option<Uuid>,
) -> Result<Folder, sqlx::Error> {
    let now = Utc::now();
    let folder = Folder {
        id: Uuid::new_v4(),
        name: name.to_string(),
        parent_id,
        created_at: now,
        updated_at: now,
    };
    sqlx::query(
        "INSERT INTO folders (id, name, parent_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(folder.id)
    .bind(&folder.name)
    .bind(folder.parent_id)
    .bind(folder.created_at)
    .bind(folder.updated_at)
    .execute(pool)
    .await?;
    Ok(folder)
}

/// Fetch a folder by id, or `None` if it does not exist.
pub async fn get_folder(pool: &SqlitePool, id: Uuid) -> Result<Option<Folder>, sqlx::Error> {
    sqlx::query_as::<_, Folder>("SELECT * FROM folders WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Total folder count (for the paginated list envelope).
pub async fn count_folders(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM folders")
        .fetch_one(pool)
        .await
}

/// One page of folders, ordered by name (case-insensitive) then creation time. The full set is small
/// -- the sidebar fetches it whole and builds the nested tree client-side.
pub async fn list_folders(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Folder>, sqlx::Error> {
    sqlx::query_as::<_, Folder>(
        "SELECT * FROM folders ORDER BY name COLLATE NOCASE, created_at LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Rename a folder, stamping `updated_at`. Returns the updated row, or `None` when no folder has that
/// id. The caller validates `name` (non-empty, length bound).
pub async fn update_folder_name(
    pool: &SqlitePool,
    id: Uuid,
    name: &str,
) -> Result<Option<Folder>, sqlx::Error> {
    let result = sqlx::query("UPDATE folders SET name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_folder(pool, id).await
}

/// Reparent a folder (`parent_id = None` moves it to the root), stamping `updated_at`. Returns the
/// updated row, or `None` when no folder has that id. The caller guards against cycles (see
/// [`folder_is_descendant`]) and validates that `parent_id` (when given) exists.
pub async fn set_folder_parent(
    pool: &SqlitePool,
    id: Uuid,
    parent_id: Option<Uuid>,
) -> Result<Option<Folder>, sqlx::Error> {
    let result = sqlx::query("UPDATE folders SET parent_id = ?, updated_at = ? WHERE id = ?")
        .bind(parent_id)
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_folder(pool, id).await
}

/// Delete a folder. Its sub-folder subtree is removed (FK `ON DELETE CASCADE`) and every meeting in
/// that subtree is un-filed (`meetings.folder_id` FK `ON DELETE SET NULL`) rather than deleted.
/// Returns whether a row was removed.
pub async fn delete_folder(pool: &SqlitePool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM folders WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Whether `candidate` is `ancestor` itself or lives somewhere in its subtree. This is the cycle
/// guard for reparenting: moving folder `X` under `candidate` is illegal when
/// `folder_is_descendant(candidate, X)` (it would place `X` inside its own subtree). Walks the
/// `parent_id` chain up from `candidate`, bounded by a visited-set so a (corrupt) pre-existing cycle
/// cannot spin the loop forever.
pub async fn folder_is_descendant(
    pool: &SqlitePool,
    candidate: Uuid,
    ancestor: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(candidate);
    while let Some(id) = current {
        if id == ancestor {
            return Ok(true);
        }
        // The reparent guard keeps the tree acyclic, but a corrupt cycle must never spin this loop
        // (it would pin a pool connection): stop the first time a folder is revisited.
        if !seen.insert(id) {
            break;
        }
        current =
            sqlx::query_scalar::<_, Option<Uuid>>("SELECT parent_id FROM folders WHERE id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?
                .flatten();
    }
    Ok(false)
}

/// File a meeting under a folder (`folder_id = None` un-files it), stamping `updated_at`. Returns the
/// updated meeting, or `None` when no meeting has that id. The caller validates that `folder_id`
/// (when given) exists.
pub async fn assign_meeting_folder(
    pool: &SqlitePool,
    meeting_id: Uuid,
    folder_id: Option<Uuid>,
) -> Result<Option<Meeting>, sqlx::Error> {
    let result = sqlx::query("UPDATE meetings SET folder_id = ?, updated_at = ? WHERE id = ?")
        .bind(folder_id)
        .bind(Utc::now())
        .bind(meeting_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_meeting(pool, meeting_id).await
}

/// Number of segments in a meeting.
pub async fn count_segments(pool: &SqlitePool, meeting_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM segments WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_one(pool)
        .await
}

/// One page of a meeting's segments, ordered by start time.
pub async fn list_segments_page(
    pool: &SqlitePool,
    meeting_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<Segment>, sqlx::Error> {
    sqlx::query_as::<_, Segment>(
        "SELECT * FROM segments WHERE meeting_id = ? ORDER BY start_s LIMIT ? OFFSET ?",
    )
    .bind(meeting_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// One transcript-search hit: the matched segment plus the meeting it belongs to and a `snippet()`
/// of the matching text. The snippet wraps each match in the private-use sentinels U+E000/U+E001
/// (`char(57344)`/`char(57345)`) so the client can highlight without any HTML in the payload.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct SearchHitRow {
    pub meeting_id: Uuid,
    pub meeting_title: String,
    pub meeting_status: MeetingStatus,
    pub started_at: DateTime<Utc>,
    pub segment_id: Uuid,
    pub stream: Stream,
    pub speaker_label: String,
    pub start_s: f64,
    pub snippet: String,
}

// WARNING: `segments_fts` is an external-content FTS5 index keyed on `segments.rowid`, and
// `segments`' primary key is a BLOB UUID (not an INTEGER PRIMARY KEY alias). SQLite reassigns such
// implicit rowids on `VACUUM`, which would desync this index from the content table and make search
// return the wrong rows. There is no `VACUUM` anywhere in the codebase today; if one is ever added
// (e.g. to compact after the cascade deletes in `delete_meeting`), it MUST be followed by
// `INSERT INTO segments_fts(segments_fts) VALUES('rebuild');` to rebuild the index.

/// Total number of segments matching an FTS5 `MATCH` query (for the paginated list envelope).
/// `match_query` is a bound parameter built by the caller from sanitized tokens.
pub async fn count_search(pool: &SqlitePool, match_query: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM segments_fts WHERE segments_fts MATCH ?")
        .bind(match_query)
        .fetch_one(pool)
        .await
}

/// One page of transcript-search hits across every meeting, ranked by FTS5 relevance (`rank`).
/// Joins the FTS index back to `segments` (for the segment + its timing/speaker) and `meetings` (for
/// the meeting context each hit is shown under).
pub async fn search_segments(
    pool: &SqlitePool,
    match_query: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<SearchHitRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchHitRow>(
        "SELECT m.id AS meeting_id, m.title AS meeting_title, m.status AS meeting_status, \
         m.started_at AS started_at, s.id AS segment_id, s.stream AS stream, \
         s.speaker_label AS speaker_label, s.start_s AS start_s, \
         snippet(segments_fts, 0, char(57344), char(57345), '…', 12) AS snippet \
         FROM segments_fts \
         JOIN segments s ON s.rowid = segments_fts.rowid \
         JOIN meetings m ON m.id = s.meeting_id \
         WHERE segments_fts MATCH ? ORDER BY rank LIMIT ? OFFSET ?",
    )
    .bind(match_query)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Overwrite a segment's transcript `text` (a manual edit), marking it `edited` and stamping
/// `updated_at`. Scoped by `meeting_id` so an id from another meeting cannot be edited via this
/// meeting's route. Returns the updated row, or `None` when no such segment exists. The FTS index
/// re-syncs automatically via the `segments_au` trigger, so the edit is immediately searchable.
pub async fn update_segment_text(
    pool: &SqlitePool,
    meeting_id: Uuid,
    segment_id: Uuid,
    text: &str,
) -> Result<Option<Segment>, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE segments SET text = ?, edited = 1, updated_at = ? WHERE id = ? AND meeting_id = ?",
    )
    .bind(text)
    .bind(Utc::now())
    .bind(segment_id)
    .bind(meeting_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    sqlx::query_as::<_, Segment>("SELECT * FROM segments WHERE id = ?")
        .bind(segment_id)
        .fetch_optional(pool)
        .await
}

/// A meeting's speaker clusters joined to their bound identity names, ordered by ordinal.
pub async fn list_speaker_rows(
    pool: &SqlitePool,
    meeting_id: Uuid,
) -> Result<Vec<SpeakerRow>, sqlx::Error> {
    sqlx::query_as::<_, SpeakerRow>(
        "SELECT c.id, c.ordinal, c.identity_id, c.locked, i.display_name \
         FROM clusters c LEFT JOIN identities i ON i.id = c.identity_id \
         WHERE c.meeting_id = ? ORDER BY c.ordinal",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await
}

/// Rename a cluster to a person: get-or-create the identity, lock the binding, and relabel that
/// speaker's already-saved segments — all in one transaction. Scoped by `meeting_id` so a cluster id
/// from another meeting cannot be touched through this meeting's route.
/// Returns the updated speaker row, or `None` if the cluster is not in that meeting.
pub async fn rename_cluster(
    pool: &SqlitePool,
    meeting_id: Uuid,
    cluster_id: Uuid,
    display_name: &str,
) -> Result<Option<SpeakerRow>, sqlx::Error> {
    let name = display_name.trim();
    let mut tx = pool.begin().await?;

    // Only `ordinal` is needed for the returned row; selecting it (not `SELECT *`) skips decoding the
    // per-cluster centroid BLOB.
    let ordinal: Option<i64> =
        sqlx::query_scalar("SELECT ordinal FROM clusters WHERE id = ? AND meeting_id = ?")
            .bind(cluster_id)
            .bind(meeting_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(ordinal) = ordinal else {
        return Ok(None);
    };

    let now = Utc::now();
    let identity_id = get_or_create_identity(&mut tx, name, now).await?;

    sqlx::query("UPDATE clusters SET identity_id = ?, locked = 1, updated_at = ? WHERE id = ?")
        .bind(identity_id)
        .bind(now)
        .bind(cluster_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE segments SET speaker_label = ?, updated_at = ? \
         WHERE cluster_id = ? AND meeting_id = ?",
    )
    .bind(name)
    .bind(now)
    .bind(cluster_id)
    .bind(meeting_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Some(SpeakerRow {
        id: cluster_id,
        ordinal,
        identity_id: Some(identity_id),
        locked: true,
        display_name: Some(name.to_string()),
    }))
}

/// Where a single line is being reassigned: an existing cluster in the same meeting, or a person by
/// name (reuse that identity's cluster in the meeting if it has one, else create a locked one).
#[derive(Debug, Clone)]
pub enum SpeakerTarget<'a> {
    Cluster(Uuid),
    Name(&'a str),
}

/// Outcome of [`reassign_segment_speaker`], mapped by the route to 200/404/422.
#[derive(Debug, Clone, PartialEq)]
pub enum ReassignOutcome {
    /// The line was reassigned; carries the updated segment row.
    Reassigned(Segment),
    /// No segment with that id exists in the meeting.
    SegmentNotFound,
    /// The line is a Me segment; only diarized Them lines have a cluster to reassign.
    NotThemStream,
    /// The target `cluster_id` does not exist in this meeting.
    ClusterNotFound,
}

/// Reassign one Them line to a different speaker: repoint its `cluster_id` and re-copy the resolved
/// `speaker_label`, marking it `edited` (so the "warn before refine" badge covers it). Scoped by
/// `meeting_id` so an id from another meeting cannot be touched. All in one transaction; a `Name`
/// target get-or-creates the identity and reuses-or-creates its cluster (mirrors [`rename_cluster`]),
/// but at the granularity of a single segment rather than the whole cluster.
pub async fn reassign_segment_speaker(
    pool: &SqlitePool,
    meeting_id: Uuid,
    segment_id: Uuid,
    target: SpeakerTarget<'_>,
) -> Result<ReassignOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let segment =
        sqlx::query_as::<_, Segment>("SELECT * FROM segments WHERE id = ? AND meeting_id = ?")
            .bind(segment_id)
            .bind(meeting_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(segment) = segment else {
        return Ok(ReassignOutcome::SegmentNotFound);
    };
    if segment.stream != Stream::Them {
        return Ok(ReassignOutcome::NotThemStream);
    }

    let now = Utc::now();
    let (cluster_id, label) = match target {
        SpeakerTarget::Cluster(target_id) => {
            let Some(label) = resolve_cluster_label(&mut tx, meeting_id, target_id).await? else {
                return Ok(ReassignOutcome::ClusterNotFound);
            };
            (target_id, label)
        }
        SpeakerTarget::Name(name) => {
            let identity_id = get_or_create_identity(&mut tx, name, now).await?;
            // Reuse this person's existing cluster in the meeting so their lines share one cluster
            // (hence one color); else create a fresh locked cluster bound to them.
            let existing: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM clusters WHERE meeting_id = ? AND identity_id = ?",
            )
            .bind(meeting_id)
            .bind(identity_id)
            .fetch_optional(&mut *tx)
            .await?;
            let cluster_id = match existing {
                Some(id) => id,
                None => {
                    let ordinal: i64 = sqlx::query_scalar(
                        "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM clusters WHERE meeting_id = ?",
                    )
                    .bind(meeting_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    let id = Uuid::new_v4();
                    sqlx::query(
                        "INSERT INTO clusters \
                         (id, meeting_id, ordinal, identity_id, locked, centroid, created_at, updated_at) \
                         VALUES (?, ?, ?, ?, 1, NULL, ?, ?)",
                    )
                    .bind(id)
                    .bind(meeting_id)
                    .bind(ordinal)
                    .bind(identity_id)
                    .bind(now)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
                    id
                }
            };
            (cluster_id, name.to_string())
        }
    };

    sqlx::query(
        "UPDATE segments SET cluster_id = ?, speaker_label = ?, edited = 1, updated_at = ? \
         WHERE id = ? AND meeting_id = ?",
    )
    .bind(cluster_id)
    .bind(&label)
    .bind(now)
    .bind(segment_id)
    .bind(meeting_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ReassignOutcome::Reassigned(Segment {
        cluster_id: Some(cluster_id),
        speaker_label: label,
        edited: true,
        updated_at: now,
        ..segment
    }))
}

/// A cluster's resolved label — its bound identity's name, else `"Speaker {ordinal}"` — or `None`
/// when the cluster is not in `meeting_id`. Shared by the single-line reassign and the whole-cluster
/// merge so the two manual-correction paths cannot drift. Takes the transaction connection so
/// callers stay atomic. Only `identity_id` + `ordinal` are selected (not `SELECT *`), which skips
/// decoding the per-cluster centroid BLOB.
async fn resolve_cluster_label(
    conn: &mut sqlx::SqliteConnection,
    meeting_id: Uuid,
    cluster_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let cluster: Option<(Option<Uuid>, i64)> =
        sqlx::query_as("SELECT identity_id, ordinal FROM clusters WHERE id = ? AND meeting_id = ?")
            .bind(cluster_id)
            .bind(meeting_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((identity_id, ordinal)) = cluster else {
        return Ok(None);
    };
    let name: Option<String> = match identity_id {
        Some(identity_id) => {
            sqlx::query_scalar("SELECT display_name FROM identities WHERE id = ?")
                .bind(identity_id)
                .fetch_optional(&mut *conn)
                .await?
        }
        None => None,
    };
    Ok(Some(name.unwrap_or_else(|| format!("Speaker {ordinal}"))))
}

/// Outcome of [`merge_clusters`], mapped by the route to 200/404/422.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The source's lines moved onto the target and the source cluster is gone.
    Merged,
    /// No cluster with that id exists in the meeting.
    SourceNotFound,
    /// The target cluster does not exist in this meeting.
    TargetNotFound,
    /// Source and target are the same cluster.
    SameCluster,
}

/// Combine two of a meeting's speakers: move every one of `source_id`'s lines onto `target_id`,
/// relabel them to the target's resolved name, mark them `edited` (a merge is a bulk reassignment),
/// and delete the now-empty source cluster. All in one transaction, scoped by `meeting_id`.
///
/// Meeting-scoped by design: the target's centroid, `locked` flag, and identity binding are left
/// exactly as they were, and no other meeting is touched. Deleting the source does discard its
/// centroid, which removes that one sample from cross-meeting recognition — intended, since a
/// spurious split should not teach the recognizer, but it is why the UI confirms first.
///
/// The source's ordinal is *not* reclaimed: the surviving speakers keep their labels (a gap reads
/// "Speaker 1, Speaker 3"). Renumbering would silently rename unrelated people whose segments still
/// carry the old `speaker_label` text.
pub async fn merge_clusters(
    pool: &SqlitePool,
    meeting_id: Uuid,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<MergeOutcome, sqlx::Error> {
    if source_id == target_id {
        return Ok(MergeOutcome::SameCluster);
    }
    let mut tx = pool.begin().await?;

    let source: Option<i64> =
        sqlx::query_scalar("SELECT ordinal FROM clusters WHERE id = ? AND meeting_id = ?")
            .bind(source_id)
            .bind(meeting_id)
            .fetch_optional(&mut *tx)
            .await?;
    if source.is_none() {
        return Ok(MergeOutcome::SourceNotFound);
    }
    let Some(label) = resolve_cluster_label(&mut tx, meeting_id, target_id).await? else {
        return Ok(MergeOutcome::TargetNotFound);
    };

    let now = Utc::now();
    // Repoint the segments BEFORE dropping the source. `segments.cluster_id` is
    // `ON DELETE SET NULL` with foreign keys on, so deleting first would silently orphan every one
    // of these lines to a NULL cluster carrying a stale label, with no way back.
    sqlx::query(
        "UPDATE segments SET cluster_id = ?, speaker_label = ?, edited = 1, updated_at = ? \
         WHERE cluster_id = ? AND meeting_id = ?",
    )
    .bind(target_id)
    .bind(&label)
    .bind(now)
    .bind(source_id)
    .bind(meeting_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM clusters WHERE id = ? AND meeting_id = ?")
        .bind(source_id)
        .bind(meeting_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(MergeOutcome::Merged)
}

/// Get an identity id by display name, creating the identity if it does not exist. Runs on a
/// transaction connection so callers stay atomic. Shared by [`rename_cluster`] and the refine's
/// locked-label carry-forward.
async fn get_or_create_identity(
    conn: &mut sqlx::SqliteConnection,
    name: &str,
    now: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    // Atomic get-or-create: a plain SELECT-then-INSERT races a concurrent refine/rename through the
    // `display_name` UNIQUE constraint, and the loser's INSERT would abort the whole transaction.
    // `ON CONFLICT DO UPDATE ... RETURNING` reuses the existing row's id in one statement; the update
    // writes `updated_at` back to its own current value, so a mere reuse never reorders
    // `list_identities` (which sorts by `updated_at`).
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO identities (id, display_name, email, created_at, updated_at) \
         VALUES (?, ?, NULL, ?, ?) \
         ON CONFLICT(display_name) DO UPDATE SET updated_at = identities.updated_at \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

/// A refined Them segment produced by the offline diarize + re-transcribe pass.
#[derive(Debug, Clone)]
pub struct RefinedThemSegment {
    pub ordinal: i64,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// How much of the Them track the refine transcribed. `fraction` is the share of *audible* time, so
/// silence does not count against it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RefineCoverage {
    pub fraction: f64,
    pub recovered_spans: usize,
    pub unrecovered_spans: usize,
}

/// Coverage below which a refine counts as truncated. Stated once; the orchestrator warns on it and
/// the API flags it to the UI.
pub const MIN_REFINE_COVERAGE: f64 = 0.8;

impl RefineCoverage {
    pub fn is_incomplete(&self) -> bool {
        self.fraction < MIN_REFINE_COVERAGE
    }
}

/// The offline refine's output persisted by [`replace_them_segments`]: the re-transcribed segments
/// plus each speaker's L2-normalized voiceprint by 1-based ordinal (empty when the diarizer emits
/// none), and how complete the transcription was.
#[derive(Debug, Clone, Default)]
pub struct RefineResult {
    pub segments: Vec<RefinedThemSegment>,
    pub centroids: HashMap<i64, Vec<f32>>,
    /// `None` when no decode ran (a default / no-speech result).
    pub coverage: Option<RefineCoverage>,
}

/// The local-LLM summarization step's output, persisted by [`upsert_meeting_notes`]: the model's
/// reply verbatim as the Markdown note. The orchestrator's [`crate::Summarizer`] analogue produces it
/// from the finalized transcript; the prompt template dictates its format, so it is a single
/// `content` field with no structured summary/action-item shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesResult {
    pub content: String,
}

/// A meeting's user-authored notes row, or `None` when the user has typed none yet.
pub async fn get_user_notes(
    pool: &SqlitePool,
    meeting_id: Uuid,
) -> Result<Option<UserNotes>, sqlx::Error> {
    sqlx::query_as::<_, UserNotes>("SELECT * FROM user_notes WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
}

/// Insert or replace a meeting's user-authored notes (one row per meeting; each autosave overwrites
/// the body). `created_at` is preserved across saves via the existing-row coalesce so the row keeps
/// its first-typed timestamp while `updated_at` advances. Returns the stored row.
pub async fn upsert_user_notes(
    pool: &SqlitePool,
    meeting_id: Uuid,
    body: &str,
) -> Result<UserNotes, sqlx::Error> {
    let now = Utc::now();
    sqlx::query_as::<_, UserNotes>(
        "INSERT INTO user_notes (meeting_id, body, created_at, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(meeting_id) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at \
         RETURNING *",
    )
    .bind(meeting_id)
    .bind(body)
    .bind(now)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Insert or replace a meeting's generated notes (one row per meeting; regenerating overwrites).
/// `content` is the model's reply stored verbatim; `model` records the GGUF that produced it.
/// `created_at` is preserved across regenerations via the upsert's `excluded`/existing coalesce so
/// the row keeps its first-produced timestamp while `updated_at` advances.
pub async fn upsert_meeting_notes(
    pool: &SqlitePool,
    meeting_id: Uuid,
    result: &NotesResult,
    model: &str,
) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO meeting_notes \
         (meeting_id, content, model, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(meeting_id) DO UPDATE SET \
         content = excluded.content, \
         model = excluded.model, updated_at = excluded.updated_at, edited = 0",
    )
    .bind(meeting_id)
    .bind(&result.content)
    .bind(model)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// A meeting's generated notes, or `None` when it has none yet.
pub async fn get_meeting_notes(
    pool: &SqlitePool,
    meeting_id: Uuid,
) -> Result<Option<MeetingNotes>, sqlx::Error> {
    sqlx::query_as::<_, MeetingNotes>("SELECT * FROM meeting_notes WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
}

/// Overwrite a meeting's notes with a manual edit: replace the `content`, mark `edited`, and stamp
/// `updated_at`. `model` is left as-is (the notes still originated from that model, now hand-corrected).
/// Returns the updated row, or `None` when the meeting has no notes row yet — editing applies only to
/// already-generated notes.
pub async fn update_meeting_notes(
    pool: &SqlitePool,
    meeting_id: Uuid,
    content: &str,
) -> Result<Option<MeetingNotes>, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE meeting_notes SET content = ?, edited = 1, updated_at = ? WHERE meeting_id = ?",
    )
    .bind(content)
    .bind(Utc::now())
    .bind(meeting_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_meeting_notes(pool, meeting_id).await
}

/// The most recent `updated_at` across a meeting's segments, or `None` when it has none. Compared
/// against a notes row's `updated_at` to tell whether the transcript changed *after* the notes were
/// generated — the "notes out of date" hint that prompts a regenerate.
pub async fn latest_segment_update(
    pool: &SqlitePool,
    meeting_id: Uuid,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar("SELECT MAX(updated_at) FROM segments WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_one(pool)
        .await
}

/// `(display_name, centroid bytes)` for every person named + locked in a *different* meeting with a
/// stored voiceprint — the candidates a refine matches a returning speaker against.
pub async fn known_voiceprints(
    pool: &SqlitePool,
    exclude_meeting_id: Uuid,
) -> Result<Vec<(String, Vec<u8>)>, sqlx::Error> {
    sqlx::query_as(KNOWN_VOICEPRINTS_SQL)
        .bind(exclude_meeting_id)
        .fetch_all(pool)
        .await
}

/// Auto-name returning speakers by matching each ordinal's voiceprint against people named + locked
/// in prior meetings (cosine `>= threshold`, the effective `speakers.recognition_threshold`). A
/// manual carry-forward (`manual`) wins, so those ordinals are skipped. Reads on the refine
/// transaction; returns `ordinal -> recognized name` (bound but *not* locked — a manual rename can
/// still override).
async fn recognize_speakers(
    conn: &mut sqlx::SqliteConnection,
    meeting_id: Uuid,
    centroids: &HashMap<i64, Vec<f32>>,
    manual: &HashMap<i64, String>,
    threshold: f64,
) -> Result<HashMap<i64, String>, sqlx::Error> {
    let mut recognized = HashMap::new();
    if centroids.is_empty() {
        return Ok(recognized);
    }
    let known_bytes: Vec<(String, Vec<u8>)> = sqlx::query_as(KNOWN_VOICEPRINTS_SQL)
        .bind(meeting_id)
        .fetch_all(&mut *conn)
        .await?;
    if known_bytes.is_empty() {
        return Ok(recognized);
    }
    let known: Vec<(String, Vec<f32>)> = known_bytes
        .into_iter()
        .map(|(name, blob)| (name, centroid_from_bytes(&blob)))
        .collect();
    // Score every ordinal's best match, then resolve so each name binds to at most one ordinal (the
    // highest-scoring), mirroring `carry_forward_locked_names`. Without this, two clusters that both
    // clear the threshold for the same person would both take that name; the runner-up now stays
    // "Speaker N" instead.
    let mut candidates: Vec<(i64, &str, f64)> = Vec::new();
    for (&ordinal, centroid) in centroids {
        if manual.contains_key(&ordinal) {
            continue; // a manual carry-forward name wins over auto-recognition
        }
        if let Some((name, score)) = best_identity(centroid, &known, threshold) {
            candidates.push((ordinal, name, score));
        }
    }
    // Highest score first; deterministic tiebreak (lower ordinal, then name).
    candidates.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
            .then(a.1.cmp(b.1))
    });
    let mut used_names: HashSet<String> = HashSet::new();
    for (ordinal, name, _score) in candidates {
        if used_names.insert(name.to_string()) {
            recognized.insert(ordinal, name.to_string());
        }
    }
    Ok(recognized)
}

/// Carry each prior *locked* manual name forward onto the new turn ordinal its old segments most
/// overlap, so a re-diarize never drops a manual binding (one name <-> one ordinal). Reads on
/// `conn` (the refine transaction) before the old clusters are dropped; returns
/// `new ordinal -> display_name`.
async fn carry_forward_locked_names(
    conn: &mut sqlx::SqliteConnection,
    meeting_id: Uuid,
    refined: &[RefinedThemSegment],
) -> Result<HashMap<i64, String>, sqlx::Error> {
    // Prior locked bindings: old cluster id -> its manual name.
    let prior: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT c.id, i.display_name FROM clusters c \
         JOIN identities i ON i.id = c.identity_id \
         WHERE c.meeting_id = ? AND c.locked = 1",
    )
    .bind(meeting_id)
    .fetch_all(&mut *conn)
    .await?;
    if prior.is_empty() {
        return Ok(HashMap::new());
    }
    let prior_name: HashMap<Uuid, String> = prior.into_iter().collect();

    // Old Them segments that were bound to a cluster (their time spans drive the vote).
    let old: Vec<(Uuid, f64, f64)> = sqlx::query_as(
        "SELECT cluster_id, start_s, end_s FROM segments \
         WHERE meeting_id = ? AND stream = ? AND cluster_id IS NOT NULL",
    )
    .bind(meeting_id)
    .bind(Stream::Them)
    .fetch_all(&mut *conn)
    .await?;

    // The refine's turns, keyed by ordinal-as-label, so `assign_segment_speaker` maps an old
    // segment's span onto the new ordinal it most overlaps (the Them channel is timeline-anchored,
    // so both are in meeting time -> offset 0).
    let turns: Vec<SpeakerTurn> = refined
        .iter()
        .map(|s| SpeakerTurn {
            speaker: s.ordinal.to_string(),
            start_s: s.start_s,
            end_s: s.end_s,
        })
        .collect();

    // Vote (new ordinal, prior name) for each old segment that carried a locked name.
    let mut votes: HashMap<(i64, String), usize> = HashMap::new();
    for (cluster_id, start_s, end_s) in old {
        let Some(name) = prior_name.get(&cluster_id) else {
            continue;
        };
        let Some(label) = assign_segment_speaker(start_s, end_s, &turns, 0.0) else {
            continue;
        };
        let Ok(ordinal) = label.parse::<i64>() else {
            continue;
        };
        *votes.entry((ordinal, name.clone())).or_insert(0) += 1;
    }

    // Resolve to one name <-> one ordinal, highest vote first (deterministic tiebreak: more votes,
    // then lower ordinal, then name).
    let mut ranked: Vec<((i64, String), usize)> = votes.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(a.0 .0.cmp(&b.0 .0))
            .then(a.0 .1.cmp(&b.0 .1))
    });
    let mut ordinal_names: HashMap<i64, String> = HashMap::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for ((ordinal, name), _votes) in ranked {
        if !ordinal_names.contains_key(&ordinal) && !used_names.contains(&name) {
            used_names.insert(name.clone());
            ordinal_names.insert(ordinal, name);
        }
    }
    Ok(ordinal_names)
}

/// Replace a meeting's Them segments + clusters with the refine's output, in one transaction: drop
/// the existing Them segments (Me is untouched) + all clusters, create one cluster per distinct
/// ordinal, and insert the refined segments bound to them. Applies, in precedence order:
/// 1. **Manual carry-forward** — a prior *locked* name is voted onto the new ordinal its old
///    segments most overlap ([`carry_forward_locked_names`]); that cluster is re-bound + re-locked.
/// 2. **Cross-meeting recognition** — an unclaimed ordinal whose voiceprint matches a person named
///    in a prior meeting ([`recognize_speakers`]) is bound to them but left *unlocked* (provisional).
/// 3. Otherwise a fresh unlocked `"Speaker N"`.
///
/// Each ordinal's voiceprint (`result.centroids`) is stored on its cluster so a later meeting can
/// recognize the speaker. `recognition_threshold` is the effective `speakers.recognition_threshold`
/// (cosine cutoff for cross-meeting recognition). A refine that produced no segments is a no-op —
/// never wipe the transcript.
pub async fn replace_them_segments(
    pool: &SqlitePool,
    meeting_id: Uuid,
    result: &RefineResult,
    recognition_threshold: f64,
) -> Result<(), sqlx::Error> {
    if result.segments.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    // Resolve names before the old clusters/segments are dropped: manual carry-forward first, then
    // recognition for the ordinals a manual name did not claim.
    let ordinal_names = carry_forward_locked_names(&mut tx, meeting_id, &result.segments).await?;
    let recognized = recognize_speakers(
        &mut tx,
        meeting_id,
        &result.centroids,
        &ordinal_names,
        recognition_threshold,
    )
    .await?;

    // Them segments reference clusters, so delete them before the clusters (Me segments are NULL).
    sqlx::query("DELETE FROM segments WHERE meeting_id = ? AND stream = ?")
        .bind(meeting_id)
        .bind(Stream::Them)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM clusters WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut *tx)
        .await?;

    let mut cluster_ids: HashMap<i64, Uuid> = HashMap::new();
    for seg in &result.segments {
        let cluster_id = match cluster_ids.get(&seg.ordinal) {
            Some(id) => *id,
            None => {
                let id = Uuid::new_v4();
                // Precedence: manual (locked) > recognized (bound, unlocked) > fresh unlocked.
                let (identity_id, locked) = if let Some(name) = ordinal_names.get(&seg.ordinal) {
                    (
                        Some(get_or_create_identity(&mut tx, name, now).await?),
                        true,
                    )
                } else if let Some(name) = recognized.get(&seg.ordinal) {
                    (
                        Some(get_or_create_identity(&mut tx, name, now).await?),
                        false,
                    )
                } else {
                    (None, false)
                };
                let centroid = result
                    .centroids
                    .get(&seg.ordinal)
                    .map(|c| centroid_to_bytes(c));
                sqlx::query(
                    "INSERT INTO clusters \
                     (id, meeting_id, ordinal, identity_id, locked, centroid, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(id)
                .bind(meeting_id)
                .bind(seg.ordinal)
                .bind(identity_id)
                .bind(locked)
                .bind(centroid)
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                cluster_ids.insert(seg.ordinal, id);
                id
            }
        };
        let label = ordinal_names
            .get(&seg.ordinal)
            .or_else(|| recognized.get(&seg.ordinal))
            .cloned()
            .unwrap_or_else(|| format!("Speaker {}", seg.ordinal));
        sqlx::query(
            "INSERT INTO segments \
             (id, meeting_id, cluster_id, stream, speaker_label, text, start_s, end_s, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4())
        .bind(meeting_id)
        .bind(cluster_id)
        .bind(Stream::Them)
        .bind(&label)
        .bind(&seg.text)
        .bind(seg.start_s)
        .bind(seg.end_s)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    // Left untouched when the refiner reported no coverage.
    if let Some(coverage) = result.coverage {
        sqlx::query("UPDATE meetings SET refine_coverage = ?, updated_at = ? WHERE id = ?")
            .bind(coverage.fraction)
            .bind(now)
            .bind(meeting_id)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Total identity count (for the paginated list envelope).
pub async fn count_identities(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM identities")
        .fetch_one(pool)
        .await
}

/// One page of known people, most-recently-updated first (rename suggestions).
///
/// The `id` tiebreaker is required, not tidiness: `updated_at` is not unique, so ordering by it
/// alone leaves the row order undefined and a paged walk can repeat one person while dropping
/// another. It also keeps the sort off `ix_identities_updated_at`, whose backward index scan has
/// been observed returning the wrong window for an `OFFSET` on this shape. Every paged query
/// ordered by a non-unique column needs the same treatment.
pub async fn list_identities(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Identity>, sqlx::Error> {
    sqlx::query_as::<_, Identity>(
        "SELECT * FROM identities ORDER BY updated_at DESC, id LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// A person joined to one of their stored voiceprints.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
struct VoiceprintJoinRow {
    identity_id: Uuid,
    display_name: String,
    email: Option<String>,
    cluster_id: Uuid,
    locked: bool,
    meeting_id: Uuid,
    meeting_title: String,
    started_at: DateTime<Utc>,
    dim: i64,
}

// The roster is gated on `EXISTS (... centroid IS NOT NULL)`: someone merely *named* — a manual
// label on a meeting that was never refined — has no embedding and is not a voiceprint. The same
// predicate appears in `count_voiceprint_people` and `list_voiceprints` and they must stay in step,
// or the page envelope's total will not match the rows it describes. It is written out in both
// rather than interpolated in, so each query stays a single static string.

/// One stored voiceprint: the centroid on a single meeting's cluster. `locked` is what decides
/// whether it is actually a recognition candidate (see `KNOWN_VOICEPRINTS_SQL`); `dimension` is the
/// embedding length (256 from FluidAudio) and
/// never cross-matches, since `cosine` returns 0.0 on a length mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceprintSample {
    pub cluster_id: Uuid,
    pub meeting_id: Uuid,
    pub meeting_title: String,
    pub started_at: DateTime<Utc>,
    pub locked: bool,
    pub dimension: i64,
}

/// A person in the voiceprint roster with every voice sample stored for them, newest meeting first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceprintPerson {
    pub identity_id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub samples: Vec<VoiceprintSample>,
}

/// How many people have at least one stored voiceprint — the total for [`list_voiceprints`]'s page
/// envelope. Deliberately not `count_identities`: the roster is voiceprints, not names.
pub async fn count_voiceprint_people(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM identities \
         WHERE EXISTS ( \
             SELECT 1 FROM clusters WHERE identity_id = identities.id AND centroid IS NOT NULL \
         )",
    )
    .fetch_one(pool)
    .await
}

/// One page of people who have a stored voiceprint, most-recently-updated first, each with all of
/// their samples (newest meeting first). Paginates over *people* via the subquery, then attaches
/// their samples, so the envelope counts people — `count_voiceprint_people` is the matching total.
///
/// Only people with an embedding appear. Clearing someone's last sample therefore drops them from
/// the roster entirely, which is the point: this list is the set of voices recognition can match
/// against, so a row that cannot match has nothing to say and no way to be acted on.
///
/// `LENGTH(c.centroid) / 4` yields the embedding dimension without ever selecting the BLOB itself.
pub async fn list_voiceprints(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<VoiceprintPerson>, sqlx::Error> {
    let rows = sqlx::query_as::<_, VoiceprintJoinRow>(
        "SELECT i.id AS identity_id, i.display_name, i.email, \
                c.id AS cluster_id, c.locked AS locked, \
                m.id AS meeting_id, m.title AS meeting_title, m.started_at AS started_at, \
                LENGTH(c.centroid) / 4 AS dim \
         FROM identities i \
         JOIN clusters c ON c.identity_id = i.id AND c.centroid IS NOT NULL \
         JOIN meetings m ON m.id = c.meeting_id \
         WHERE i.id IN ( \
             SELECT id FROM identities \
             WHERE EXISTS ( \
                 SELECT 1 FROM clusters WHERE identity_id = identities.id AND centroid IS NOT NULL \
             ) \
             ORDER BY updated_at DESC, id LIMIT ? OFFSET ? \
         ) \
         ORDER BY i.updated_at DESC, i.id, m.started_at DESC",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    // The ORDER BY keeps one person's rows contiguous, so a single pass groups them without a map.
    let mut people: Vec<VoiceprintPerson> = Vec::new();
    for row in rows {
        if people.last().map(|p| p.identity_id) != Some(row.identity_id) {
            people.push(VoiceprintPerson {
                identity_id: row.identity_id,
                display_name: row.display_name,
                email: row.email,
                samples: Vec::new(),
            });
        }
        let person = people.last_mut().expect("pushed above");
        person.samples.push(VoiceprintSample {
            cluster_id: row.cluster_id,
            meeting_id: row.meeting_id,
            meeting_title: row.meeting_title,
            started_at: row.started_at,
            locked: row.locked,
            dimension: row.dim,
        });
    }
    Ok(people)
}

/// Outcome of [`rename_identity`], mapped by the route to 200/404/409.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameIdentityOutcome {
    /// Renamed; carries the updated row and the meetings whose transcripts now need re-exporting.
    Renamed {
        identity: Identity,
        meeting_ids: Vec<Uuid>,
    },
    /// No identity with that id.
    NotFound,
    /// Another person already holds that name (`identities.display_name` is UNIQUE).
    NameTaken,
}

/// Rename a person everywhere: the identity row plus the `speaker_label` of every segment in every
/// cluster bound to them, in one transaction. Returns the affected meeting ids so the caller can
/// re-export their transcripts.
///
/// Segments are matched by cluster, never by their old label text — a label sweep would also catch
/// segments that merely happen to share the string, and there is no upside since a cluster is the
/// only thing that binds a line to a person.
///
/// The `display_name` UNIQUE constraint is pre-checked inside the transaction rather than left to
/// the driver, so a collision is a clean [`RenameIdentityOutcome::NameTaken`] (409) instead of a
/// database error surfacing as a 500. Renaming to the name already held is a no-op.
pub async fn rename_identity(
    pool: &SqlitePool,
    identity_id: Uuid,
    display_name: &str,
) -> Result<RenameIdentityOutcome, sqlx::Error> {
    let name = display_name.trim();
    let mut tx = pool.begin().await?;

    let existing = sqlx::query_as::<_, Identity>("SELECT * FROM identities WHERE id = ?")
        .bind(identity_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(existing) = existing else {
        return Ok(RenameIdentityOutcome::NotFound);
    };
    if existing.display_name == name {
        return Ok(RenameIdentityOutcome::Renamed {
            identity: existing,
            meeting_ids: Vec::new(),
        });
    }

    let taken: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM identities WHERE display_name = ? AND id != ?")
            .bind(name)
            .bind(identity_id)
            .fetch_optional(&mut *tx)
            .await?;
    if taken.is_some() {
        return Ok(RenameIdentityOutcome::NameTaken);
    }

    let meeting_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT meeting_id FROM clusters WHERE identity_id = ?")
            .bind(identity_id)
            .fetch_all(&mut *tx)
            .await?;

    let now = Utc::now();
    sqlx::query(
        "UPDATE segments SET speaker_label = ?, updated_at = ? \
         WHERE cluster_id IN (SELECT id FROM clusters WHERE identity_id = ?)",
    )
    .bind(name)
    .bind(now)
    .bind(identity_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE identities SET display_name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(now)
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(RenameIdentityOutcome::Renamed {
        identity: Identity {
            display_name: name.to_string(),
            updated_at: now,
            ..existing
        },
        meeting_ids,
    })
}

/// Forget one stored voiceprint: clear that cluster's centroid so it stops being a cross-meeting
/// recognition candidate. The cluster, its identity binding, its `locked` flag, and its segments all
/// survive — past transcripts keep the person's name. Returns whether the cluster exists.
///
/// The `WHERE` deliberately does not require a non-NULL centroid, so repeating the call stays an
/// idempotent success rather than becoming a 404.
pub async fn clear_cluster_centroid(
    pool: &SqlitePool,
    cluster_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE clusters SET centroid = NULL, updated_at = ? WHERE id = ?")
        .bind(Utc::now())
        .bind(cluster_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Forget every voiceprint stored for a person, leaving their name on every past transcript.
/// Returns `false` when no such identity exists.
///
/// Clears *all* their centroids, not only the `locked` ones: an unlocked recognition-bound cluster's
/// centroid would otherwise survive and re-enter the candidate set the moment anyone renamed that
/// cluster.
pub async fn forget_identity_voice(
    pool: &SqlitePool,
    identity_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM identities WHERE id = ?")
        .bind(identity_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Ok(false);
    }
    sqlx::query("UPDATE clusters SET centroid = NULL, updated_at = ? WHERE identity_id = ?")
        .bind(Utc::now())
        .bind(identity_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// The stored JSON for a settings `section`, or `None` when unset (the caller uses the config
/// default).
pub async fn get_preference(
    pool: &SqlitePool,
    section: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT value FROM preferences WHERE section = ?")
        .bind(section)
        .fetch_optional(pool)
        .await
}

/// Upsert one settings `section`'s JSON (one row per section).
pub async fn set_preference(
    pool: &SqlitePool,
    section: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO preferences (id, section, value, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(section) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(section)
    .bind(value)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete one settings `section`'s stored override (a no-op when unset), reverting the effective
/// value to the config default. Used by "reset to default" actions where the default may be a
/// relative/bundled path that the section's own input validation would reject on a re-write.
pub async fn clear_preference(pool: &SqlitePool, section: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM preferences WHERE section = ?")
        .bind(section)
        .execute(pool)
        .await?;
    Ok(())
}

/// Settings sections persisted in the `preferences` table (one JSON row each). The section name is
/// the wire contract shared by the API writer (`hearsay-core`'s settings routes) and the runtime
/// readers (the `effective_*` resolvers below, called by the orchestrator at meeting start/stop).
pub const SECTION_RECORDING: &str = "recording";
pub const SECTION_SPEAKERS: &str = "speakers";
pub const SECTION_STORAGE: &str = "storage";
pub const SECTION_MODELS: &str = "models";
/// Not an editable settings panel: first-run model setup records here that it finished.
pub const SECTION_SETUP: &str = "setup";

/// The parsed JSON object for a stored section, or `None` when unset or unparseable (the caller then
/// uses its config default). A corrupt row degrades to the default rather than failing an operation.
async fn section_object(
    pool: &SqlitePool,
    section: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, sqlx::Error> {
    let Some(raw) = get_preference(pool, section).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.as_object().cloned()))
}

/// A stored settings section, read field by field against a config default.
///
/// Every accessor falls back independently, and an absent section, a corrupt one, a missing key, and
/// a wrong-typed value are all the same answer: the default. That is what lets a row written before
/// a key existed — or the partial section [`set_notes_model`] writes — still resolve, where a strict
/// struct deserialize would reject it and 500 the settings route.
pub struct Section(Option<serde_json::Map<String, serde_json::Value>>);

impl Section {
    /// Read this section from the `preferences` table.
    pub async fn load(pool: &SqlitePool, section: &str) -> Result<Self, sqlx::Error> {
        Ok(Section(section_object(pool, section).await?))
    }

    fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.as_ref().and_then(|o| o.get(key))
    }

    pub fn bool_field(&self, key: &str, default: bool) -> bool {
        self.get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    }

    pub fn u64_field(&self, key: &str, default: u64) -> u64 {
        self.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(default)
    }

    pub fn f64_field(&self, key: &str, default: f64) -> f64 {
        self.get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default)
    }

    /// A stored string, treating empty/whitespace as unset so a cleared field reverts to `default`.
    pub fn string_field(&self, key: &str, default: &str) -> String {
        self.get(key)
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    /// A stored path, treating an empty value as unset so a cleared field reverts to `default`.
    pub fn path_field(&self, key: &str, default: &Path) -> PathBuf {
        self.get(key)
            .and_then(|v| v.as_str().map(PathBuf::from))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| default.to_path_buf())
    }
}

/// Effective `record` (keep one WAV per meeting): the stored `recording` override, else `default`.
pub async fn effective_record(pool: &SqlitePool, default: bool) -> Result<bool, sqlx::Error> {
    Ok(Section::load(pool, SECTION_RECORDING)
        .await?
        .bool_field("record", default))
}

/// Effective inactivity-watchdog settings (prompt + auto-end toggles, prompt/end minutes): the stored
/// `recording` override else the config `default_*`. Each field falls back independently, so a row
/// that predates these keys (only `record`) still yields usable values. Read at meeting start so a
/// Settings change takes effect on the next meeting. Returns
/// `(prompt_enabled, auto_end_enabled, prompt_minutes, end_minutes)`.
pub async fn effective_inactivity(
    pool: &SqlitePool,
    default_prompt: bool,
    default_auto_end: bool,
    default_prompt_minutes: u64,
    default_end_minutes: u64,
) -> Result<(bool, bool, u64, u64), sqlx::Error> {
    let s = Section::load(pool, SECTION_RECORDING).await?;
    Ok((
        s.bool_field("inactivity_prompt_enabled", default_prompt),
        s.bool_field("inactivity_auto_end_enabled", default_auto_end),
        s.u64_field("inactivity_prompt_minutes", default_prompt_minutes),
        s.u64_field("inactivity_end_minutes", default_end_minutes),
    ))
}

/// Effective recordings root for a NEW meeting: the stored `storage` override, else `default`. Each
/// meeting pins its own absolute dir at creation, so changing this never orphans existing meetings.
pub async fn effective_output_dir(
    pool: &SqlitePool,
    default: &Path,
) -> Result<PathBuf, sqlx::Error> {
    Ok(Section::load(pool, SECTION_STORAGE)
        .await?
        .path_field("output_dir", default))
}

/// Effective audio-compression settings (archive the recorded WAV as lossless FLAC once a meeting is
/// this many days old): the stored `storage` override, else the config defaults. Each field falls
/// back independently, so a row that predates these keys (only `output_dir`) still yields usable
/// values. Read fresh on each sweep, so a Settings change applies without a restart. Returns
/// `(enabled, after_days)`.
pub async fn effective_compression(
    pool: &SqlitePool,
    default_enabled: bool,
    default_days: u64,
) -> Result<(bool, u64), sqlx::Error> {
    let s = Section::load(pool, SECTION_STORAGE).await?;
    Ok((
        s.bool_field("compress_audio", default_enabled),
        s.u64_field("compress_after_days", default_days),
    ))
}

/// Effective offline-refine whisper model: the stored `models` override, else `default` (the
/// bundled model from config). Read fresh at each refine, so pointing the `models` section at a
/// larger downloaded model takes effect on the next refine/rediarize with no restart.
pub async fn effective_refine_model(
    pool: &SqlitePool,
    default: &Path,
) -> Result<PathBuf, sqlx::Error> {
    Ok(Section::load(pool, SECTION_MODELS)
        .await?
        .path_field("refine_model", default))
}

/// Set the `models` section's `notes_model` to `path` (what the download manager calls on a
/// completed download), preserving the section's other fields (`refine_model`, `notes_enabled`) by
/// merging into the stored object rather than overwriting it.
pub async fn set_notes_model(pool: &SqlitePool, path: &str) -> Result<(), sqlx::Error> {
    // Atomic single-statement merge (not read-modify-write): `json_set` updates only `$.notes_model`
    // in place, preserving the section's other fields, so a background download completing here
    // cannot race a concurrent settings write into a lost update.
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO preferences (id, section, value, created_at, updated_at) \
         VALUES (?, ?, json_object('notes_model', ?), ?, ?) \
         ON CONFLICT(section) DO UPDATE SET \
             value = json_set(preferences.value, '$.notes_model', ?), \
             updated_at = excluded.updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(SECTION_MODELS)
    .bind(path)
    .bind(now)
    .bind(now)
    .bind(path)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether first-run model setup has completed on this install. Paired with the on-disk probe in
/// `hearsay-core`'s setup manager, which covers an install that already had its models.
pub async fn models_ready(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    Ok(Section::load(pool, SECTION_SETUP)
        .await?
        .bool_field("models_ready", false))
}

/// Record that first-run model setup finished.
pub async fn set_models_ready(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    set_preference(pool, SECTION_SETUP, r#"{"models_ready":true}"#).await
}

/// Effective notes settings from the same `models` section: `(notes_enabled, notes_model)`. Each
/// field falls back independently to its config default (a partial/corrupt row still yields usable
/// values), matching [`effective_speakers`]. An empty stored `notes_model` is treated as unset. Read
/// fresh at each stop/generate so a Settings or download change applies with no restart.
pub async fn effective_notes(
    pool: &SqlitePool,
    default_enabled: bool,
    default_model: &Path,
) -> Result<(bool, PathBuf), sqlx::Error> {
    let s = Section::load(pool, SECTION_MODELS).await?;
    Ok((
        s.bool_field("notes_enabled", default_enabled),
        s.path_field("notes_model", default_model),
    ))
}

/// Effective notes prompt template from the same `models` section: the stored `notes_prompt`
/// override, else `default`. An empty/whitespace stored value is treated as unset (fall back to the
/// default), mirroring [`effective_notes`]'s handling of an empty `notes_model`. Read fresh at each
/// generate so a Settings change applies with no restart.
pub async fn effective_notes_prompt(
    pool: &SqlitePool,
    default: &str,
) -> Result<String, sqlx::Error> {
    Ok(Section::load(pool, SECTION_MODELS)
        .await?
        .string_field("notes_prompt", default))
}

/// Effective `(auto_refine, recognition_threshold)`: the stored `speakers` override per field, else
/// the matching default. Each field falls back independently, so a partial/corrupt row still yields
/// usable values.
pub async fn effective_speakers(
    pool: &SqlitePool,
    default_auto_refine: bool,
    default_threshold: f64,
) -> Result<(bool, f64), sqlx::Error> {
    let s = Section::load(pool, SECTION_SPEAKERS).await?;
    Ok((
        s.bool_field("auto_refine", default_auto_refine),
        s.f64_field("recognition_threshold", default_threshold),
    ))
}
