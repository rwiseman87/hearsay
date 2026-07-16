//! Typed queries over the schema. Runtime-checked (`sqlx::query`/`query_as`); the compile-time
//! `query!` macros (offline `.sqlx` cache) are a future upgrade. Grows as the services are ported.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use hearsay_attribution::{
    assign_segment_speaker, centroid_from_bytes, centroid_to_bytes, match_identity, SpeakerTurn,
};
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

/// SQL for the known cross-meeting voiceprints: every person named + locked in another meeting with
/// a stored centroid.
const KNOWN_VOICEPRINTS_SQL: &str = "SELECT i.display_name, c.centroid FROM clusters c \
     JOIN identities i ON i.id = c.identity_id \
     WHERE c.locked = 1 AND c.centroid IS NOT NULL AND c.meeting_id != ?";

use crate::models::{
    Cluster, Folder, Identity, Meeting, MeetingNotes, MeetingStatus, Segment, Stream,
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

/// Total meeting count (for the paginated list envelope).
pub async fn count_meetings(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM meetings")
        .fetch_one(pool)
        .await
}

/// One page of meetings, most-recently-started first.
pub async fn list_meetings(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Meeting>, sqlx::Error> {
    sqlx::query_as::<_, Meeting>("SELECT * FROM meetings ORDER BY started_at DESC LIMIT ? OFFSET ?")
        .bind(limit)
        .bind(offset)
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
/// `parent_id` chain up from `candidate`; the loop is bounded by the tree depth.
pub async fn folder_is_descendant(
    pool: &SqlitePool,
    candidate: Uuid,
    ancestor: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut current = Some(candidate);
    while let Some(id) = current {
        if id == ancestor {
            return Ok(true);
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
/// speaker's already-saved segments — all in one transaction (mirrors `SpeakerService.bind_cluster`).
/// Returns the updated speaker row, or `None` if the cluster does not exist.
pub async fn rename_cluster(
    pool: &SqlitePool,
    cluster_id: Uuid,
    display_name: &str,
) -> Result<Option<SpeakerRow>, sqlx::Error> {
    let name = display_name.trim();
    let mut tx = pool.begin().await?;

    let cluster = sqlx::query_as::<_, Cluster>("SELECT * FROM clusters WHERE id = ?")
        .bind(cluster_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(cluster) = cluster else {
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
    sqlx::query("UPDATE segments SET speaker_label = ?, updated_at = ? WHERE cluster_id = ?")
        .bind(name)
        .bind(now)
        .bind(cluster_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(Some(SpeakerRow {
        id: cluster_id,
        ordinal: cluster.ordinal,
        identity_id: Some(identity_id),
        locked: true,
        display_name: Some(name.to_string()),
    }))
}

/// Get an identity id by display name, creating the identity if it does not exist. Runs on a
/// transaction connection so callers stay atomic. Shared by [`rename_cluster`] and the refine's
/// locked-label carry-forward.
async fn get_or_create_identity(
    conn: &mut sqlx::SqliteConnection,
    name: &str,
    now: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM identities WHERE display_name = ?")
            .bind(name)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities (id, display_name, email, created_at, updated_at) \
         VALUES (?, ?, NULL, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
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

/// The offline refine's output persisted by [`replace_them_segments`]: the re-transcribed segments
/// plus each speaker's L2-normalized voiceprint by 1-based ordinal (empty when the diarizer emits
/// none). Mirrors the `turn_segments` + `ordinal_centroids` the Python `apply_turn_diarization` takes.
#[derive(Debug, Clone, Default)]
pub struct RefineResult {
    pub segments: Vec<RefinedThemSegment>,
    pub centroids: HashMap<i64, Vec<f32>>,
}

/// The local-LLM summarization step's output, persisted by [`upsert_meeting_notes`]: a short summary
/// plus a flat list of action items. The orchestrator's [`crate::Summarizer`] analogue produces it
/// from the finalized transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesResult {
    pub summary: String,
    pub action_items: Vec<String>,
}

/// Insert or replace a meeting's generated notes (one row per meeting; regenerating overwrites).
/// `action_items` is stored as a JSON array of strings; `model` records the GGUF that produced it.
/// `created_at` is preserved across regenerations via the upsert's `excluded`/existing coalesce so
/// the row keeps its first-produced timestamp while `updated_at` advances.
pub async fn upsert_meeting_notes(
    pool: &SqlitePool,
    meeting_id: Uuid,
    result: &NotesResult,
    model: &str,
) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    let action_items = serde_json::to_string(&result.action_items)
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "INSERT INTO meeting_notes \
         (meeting_id, summary, action_items, model, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(meeting_id) DO UPDATE SET \
         summary = excluded.summary, action_items = excluded.action_items, \
         model = excluded.model, updated_at = excluded.updated_at",
    )
    .bind(meeting_id)
    .bind(&result.summary)
    .bind(&action_items)
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

/// `(display_name, centroid bytes)` for every person named + locked in a *different* meeting with a
/// stored voiceprint — the candidates a refine matches a returning speaker against. Port of
/// `SpeakerService.known_voiceprints`.
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
/// still override). Port of `refine.py::_recognize_speakers`.
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
    for (&ordinal, centroid) in centroids {
        if manual.contains_key(&ordinal) {
            continue; // a manual carry-forward name wins over auto-recognition
        }
        if let Some(name) = match_identity(centroid, &known, threshold) {
            recognized.insert(ordinal, name.to_string());
        }
    }
    Ok(recognized)
}

/// Carry each prior *locked* manual name forward onto the new turn ordinal its old segments most
/// overlap, so a re-diarize never drops a manual binding (one name <-> one ordinal). Port of
/// `refine.py::_carry_forward_names`. Reads on `conn` (the refine transaction) before the old
/// clusters are dropped; returns `new ordinal -> display_name`.
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
/// (cosine cutoff for cross-meeting recognition). Mirrors `refine.py` +
/// `SpeakerService.apply_turn_diarization`. A refine that produced no segments is a no-op — never
/// wipe the transcript.
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
pub async fn list_identities(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Identity>, sqlx::Error> {
    sqlx::query_as::<_, Identity>(
        "SELECT * FROM identities ORDER BY updated_at DESC LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// The stored JSON for a settings `section`, or `None` when unset (the caller uses the config
/// default). Port of `SettingsService._section`.
pub async fn get_preference(
    pool: &SqlitePool,
    section: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT value FROM preferences WHERE section = ?")
        .bind(section)
        .fetch_optional(pool)
        .await
}

/// Upsert one settings `section`'s JSON (one row per section). Port of `SettingsService._upsert`.
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

/// Effective `record` (keep one WAV per meeting): the stored `recording` override, else `default`.
pub async fn effective_record(pool: &SqlitePool, default: bool) -> Result<bool, sqlx::Error> {
    Ok(section_object(pool, SECTION_RECORDING)
        .await?
        .and_then(|o| o.get("record").and_then(serde_json::Value::as_bool))
        .unwrap_or(default))
}

/// Effective recordings root for a NEW meeting: the stored `storage` override, else `default`. Each
/// meeting pins its own absolute dir at creation, so changing this never orphans existing meetings.
pub async fn effective_output_dir(
    pool: &SqlitePool,
    default: &Path,
) -> Result<PathBuf, sqlx::Error> {
    Ok(section_object(pool, SECTION_STORAGE)
        .await?
        .and_then(|o| {
            o.get("output_dir")
                .and_then(|v| v.as_str().map(PathBuf::from))
        })
        .unwrap_or_else(|| default.to_path_buf()))
}

/// Effective offline-refine whisper model: the stored `models` override, else `default` (the
/// bundled model from config). Read fresh at each refine, so pointing the `models` section at a
/// larger downloaded model takes effect on the next refine/rediarize with no restart.
pub async fn effective_refine_model(
    pool: &SqlitePool,
    default: &Path,
) -> Result<PathBuf, sqlx::Error> {
    Ok(section_object(pool, SECTION_MODELS)
        .await?
        .and_then(|o| {
            o.get("refine_model")
                .and_then(|v| v.as_str().map(PathBuf::from))
        })
        .unwrap_or_else(|| default.to_path_buf()))
}

/// The stored `models` section as a raw JSON object (`None` when unset/corrupt), for the settings
/// API to resolve each field against its own config default. Tolerates a *partial* section — e.g.
/// the one [`set_notes_model`] writes with only `notes_model` — which a strict struct deserialize
/// would reject, 500-ing `GET /settings` after a first download on an otherwise-default install.
pub async fn models_section(
    pool: &SqlitePool,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, sqlx::Error> {
    section_object(pool, SECTION_MODELS).await
}

/// Set the `models` section's `notes_model` to `path` (what the download manager calls on a
/// completed download), preserving the section's other fields (`refine_model`, `notes_enabled`) by
/// merging into the stored object rather than overwriting it.
pub async fn set_notes_model(pool: &SqlitePool, path: &str) -> Result<(), sqlx::Error> {
    let mut obj = section_object(pool, SECTION_MODELS)
        .await?
        .unwrap_or_default();
    obj.insert(
        "notes_model".to_string(),
        serde_json::Value::String(path.to_string()),
    );
    let json = serde_json::to_string(&serde_json::Value::Object(obj))
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    set_preference(pool, SECTION_MODELS, &json).await
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
    let obj = section_object(pool, SECTION_MODELS).await?;
    let enabled = obj
        .as_ref()
        .and_then(|o| o.get("notes_enabled").and_then(serde_json::Value::as_bool))
        .unwrap_or(default_enabled);
    let model = obj
        .as_ref()
        .and_then(|o| {
            o.get("notes_model")
                .and_then(|v| v.as_str().map(PathBuf::from))
        })
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| default_model.to_path_buf());
    Ok((enabled, model))
}

/// Effective `(auto_refine, recognition_threshold)`: the stored `speakers` override per field, else
/// the matching default. Each field falls back independently, so a partial/corrupt row still yields
/// usable values.
pub async fn effective_speakers(
    pool: &SqlitePool,
    default_auto_refine: bool,
    default_threshold: f64,
) -> Result<(bool, f64), sqlx::Error> {
    let obj = section_object(pool, SECTION_SPEAKERS).await?;
    let auto_refine = obj
        .as_ref()
        .and_then(|o| o.get("auto_refine").and_then(serde_json::Value::as_bool))
        .unwrap_or(default_auto_refine);
    let threshold = obj
        .as_ref()
        .and_then(|o| {
            o.get("recognition_threshold")
                .and_then(serde_json::Value::as_f64)
        })
        .unwrap_or(default_threshold);
    Ok((auto_refine, threshold))
}
