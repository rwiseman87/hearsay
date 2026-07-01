//! Schema + query round-trip tests against an in-memory SQLite database.
//!
//! A single-connection memory pool is used so every query hits the same database (each
//! connection to `sqlite::memory:` is otherwise a distinct database).

use hearsay_db::models::{MeetingStatus, Stream};
use hearsay_db::{connect_options, queries, MIGRATOR};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options("sqlite::memory:").unwrap())
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn meeting_and_segments_roundtrip_ordered() {
    let pool = memory_pool().await;
    let started = chrono::Utc::now();
    let meeting = queries::create_meeting(&pool, "Standup", "/tmp/standup", started)
        .await
        .unwrap();
    assert_eq!(meeting.status, MeetingStatus::Recording);

    // Insert out of order; list_segments must return them ordered by start_s.
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "hello",
        5.0,
        6.0,
    )
    .await
    .unwrap();
    queries::insert_segment(&pool, meeting.id, Stream::Me, "Me", "hi", 0.0, 1.0)
        .await
        .unwrap();

    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].start_s, 0.0);
    assert_eq!(segments[0].stream, Stream::Me);
    assert_eq!(segments[1].start_s, 5.0);

    let fetched = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched, meeting);
}

#[tokio::test]
async fn finalize_sets_status_and_end() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    let ended = chrono::Utc::now();
    queries::finalize_meeting(&pool, meeting.id, ended)
        .await
        .unwrap();
    let fetched = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.status, MeetingStatus::Finalized);
    assert!(fetched.ended_at.is_some());
}

#[tokio::test]
async fn enum_stored_as_lowercase_text() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM meetings WHERE id = ?")
        .bind(meeting.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "recording");
}

#[tokio::test]
async fn foreign_key_cascade_deletes_segments() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    queries::insert_segment(&pool, meeting.id, Stream::Me, "Me", "x", 0.0, 1.0)
        .await
        .unwrap();
    sqlx::query("DELETE FROM meetings WHERE id = ?")
        .bind(meeting.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(queries::list_segments(&pool, meeting.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn identity_display_name_is_unique() {
    let pool = memory_pool().await;
    queries::create_identity(&pool, "Ada", Some("ada@example.com"))
        .await
        .unwrap();
    let dup = queries::create_identity(&pool, "Ada", None).await;
    assert!(dup.is_err());
}

#[tokio::test]
async fn cluster_centroid_blob_roundtrips() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    let centroid = vec![1u8, 2, 3, 4, 250, 255];
    let cluster = queries::create_cluster(&pool, meeting.id, 1, false, Some(centroid.clone()))
        .await
        .unwrap();
    assert_eq!(cluster.centroid, Some(centroid));
    assert!(!cluster.locked);
}
