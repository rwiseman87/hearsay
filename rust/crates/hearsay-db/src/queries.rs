//! Typed queries over the schema. Runtime-checked (`sqlx::query`/`query_as`); the compile-time
//! `query!` macros (offline `.sqlx` cache) are a future upgrade. Grows as the services are ported.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::models::{Cluster, Identity, Meeting, MeetingStatus, Segment, Stream};

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

/// Create a `recording` meeting and return the inserted row.
pub async fn create_meeting(
    pool: &SqlitePool,
    title: &str,
    folder: &str,
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
    };
    sqlx::query(
        "INSERT INTO meetings \
         (id, title, folder, status, started_at, ended_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(meeting.id)
    .bind(&meeting.title)
    .bind(&meeting.folder)
    .bind(meeting.status)
    .bind(meeting.started_at)
    .bind(meeting.ended_at)
    .bind(meeting.created_at)
    .bind(meeting.updated_at)
    .execute(pool)
    .await?;
    Ok(meeting)
}

/// Fetch a meeting by id, or `None` if it does not exist.
pub async fn get_meeting(pool: &SqlitePool, id: Uuid) -> Result<Option<Meeting>, sqlx::Error> {
    sqlx::query_as::<_, Meeting>("SELECT * FROM meetings WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Mark a meeting `finalized` with its end time.
pub async fn finalize_meeting(
    pool: &SqlitePool,
    id: Uuid,
    ended_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE meetings SET status = ?, ended_at = ?, updated_at = ? WHERE id = ?")
        .bind(MeetingStatus::Finalized)
        .bind(ended_at)
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Insert a transcript segment and return the inserted row.
#[allow(clippy::too_many_arguments)]
pub async fn insert_segment(
    pool: &SqlitePool,
    meeting_id: Uuid,
    stream: Stream,
    speaker_label: &str,
    text: &str,
    start_s: f64,
    end_s: f64,
) -> Result<Segment, sqlx::Error> {
    let now = Utc::now();
    let segment = Segment {
        id: Uuid::new_v4(),
        meeting_id,
        cluster_id: None,
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
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM identities WHERE display_name = ?")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
    let identity_id = match existing {
        Some(id) => id,
        None => {
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO identities (id, display_name, email, created_at, updated_at) \
                 VALUES (?, ?, NULL, ?, ?)",
            )
            .bind(id)
            .bind(name)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            id
        }
    };

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
