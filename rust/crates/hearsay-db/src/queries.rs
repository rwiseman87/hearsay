//! Typed queries over the schema. Runtime-checked (`sqlx::query`/`query_as`); the compile-time
//! `query!` macros (offline `.sqlx` cache) are a future upgrade. Grows as the services are ported.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::{Cluster, Identity, Meeting, MeetingStatus, Segment, Stream};

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
