//! Schema + query round-trip tests against an in-memory SQLite database.
//!
//! A single-connection memory pool is used so every query hits the same database (each
//! connection to `sqlite::memory:` is otherwise a distinct database).

use std::collections::HashMap;

use hearsay_attribution::{centroid_from_bytes, centroid_to_bytes};
use hearsay_db::models::{MeetingStatus, Stream};
use hearsay_db::queries::{RefineResult, RefinedThemSegment};
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

/// Wrap refined segments (no voiceprints) as a [`RefineResult`] for `replace_them_segments`.
fn refine_result(segments: Vec<RefinedThemSegment>) -> RefineResult {
    RefineResult {
        segments,
        ..Default::default()
    }
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
        None,
    )
    .await
    .unwrap();
    queries::insert_segment(&pool, meeting.id, Stream::Me, "Me", "hi", 0.0, 1.0, None)
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
    queries::insert_segment(&pool, meeting.id, Stream::Me, "Me", "x", 0.0, 1.0, None)
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

#[tokio::test]
async fn list_meetings_paginates_newest_first() {
    let pool = memory_pool().await;
    let base = chrono::Utc::now();
    for (i, title) in ["oldest", "middle", "newest"].iter().enumerate() {
        let started = base + chrono::Duration::seconds(i as i64);
        queries::create_meeting(&pool, title, title, started)
            .await
            .unwrap();
    }
    assert_eq!(queries::count_meetings(&pool).await.unwrap(), 3);

    let page1 = queries::list_meetings(&pool, 2, 0).await.unwrap();
    assert_eq!(
        page1.iter().map(|m| m.title.as_str()).collect::<Vec<_>>(),
        ["newest", "middle"]
    );
    let page2 = queries::list_meetings(&pool, 2, 2).await.unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].title, "oldest");
}

#[tokio::test]
async fn rename_cluster_binds_relabels_and_joins() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    let cluster = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    let segment = queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "hi",
        0.0,
        1.0,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE segments SET cluster_id = ? WHERE id = ?")
        .bind(cluster.id)
        .bind(segment.id)
        .execute(&pool)
        .await
        .unwrap();

    let renamed = queries::rename_cluster(&pool, cluster.id, "  Zed  ")
        .await
        .unwrap()
        .expect("cluster exists");
    assert_eq!(renamed.display_name.as_deref(), Some("Zed"));
    assert!(renamed.locked);

    // The join surfaces the bound name; the segment is relabelled; the identity exists once.
    let rows = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    assert_eq!(rows[0].display_name.as_deref(), Some("Zed"));
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert_eq!(segments[0].speaker_label, "Zed");
    assert_eq!(queries::count_identities(&pool).await.unwrap(), 1);

    assert!(queries::rename_cluster(&pool, uuid::Uuid::new_v4(), "X")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn delete_meeting_removes_clusters_and_reports_missing() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();

    assert!(queries::delete_meeting(&pool, meeting.id).await.unwrap());
    assert!(queries::list_speaker_rows(&pool, meeting.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(queries::count_meetings(&pool).await.unwrap(), 0);
    assert!(!queries::delete_meeting(&pool, meeting.id).await.unwrap());
}

#[tokio::test]
async fn replace_them_segments_swaps_clusters_keeps_me() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    // Seed: a Me segment (untouched by the refine) + an old Them segment on an old cluster.
    queries::insert_segment(&pool, meeting.id, Stream::Me, "Me", "hi", 0.0, 1.0, None)
        .await
        .unwrap();
    let old = queries::create_cluster(&pool, meeting.id, 5, false, None)
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 5",
        "old",
        0.0,
        1.0,
        Some(old.id),
    )
    .await
    .unwrap();

    let refined = vec![
        queries::RefinedThemSegment {
            ordinal: 1,
            text: "hello".into(),
            start_s: 0.0,
            end_s: 2.0,
        },
        queries::RefinedThemSegment {
            ordinal: 2,
            text: "world".into(),
            start_s: 2.0,
            end_s: 4.0,
        },
        queries::RefinedThemSegment {
            ordinal: 1,
            text: "again".into(),
            start_s: 4.0,
            end_s: 5.0,
        },
    ];
    queries::replace_them_segments(&pool, meeting.id, &refine_result(refined))
        .await
        .unwrap();

    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let me: Vec<_> = segments.iter().filter(|s| s.stream == Stream::Me).collect();
    let them: Vec<_> = segments
        .iter()
        .filter(|s| s.stream == Stream::Them)
        .collect();
    assert_eq!(me.len(), 1, "Me segment preserved");
    assert_eq!(them.len(), 3, "old Them replaced by 3 refined");
    assert!(them.iter().all(|s| s.cluster_id.is_some()));

    // One cluster per distinct ordinal; the two ordinal-1 segments share it.
    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    assert_eq!(speakers.len(), 2);
    let ord1: Vec<_> = them
        .iter()
        .filter(|s| s.speaker_label == "Speaker 1")
        .collect();
    assert_eq!(ord1.len(), 2);
    assert_eq!(ord1[0].cluster_id, ord1[1].cluster_id);
}

#[tokio::test]
async fn replace_them_segments_carries_forward_locked_names() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    // An old Them cluster (ordinal 5) with a segment late in the meeting, manually named + locked.
    let old = queries::create_cluster(&pool, meeting.id, 5, false, None)
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 5",
        "old",
        3.0,
        4.0,
        Some(old.id),
    )
    .await
    .unwrap();
    queries::rename_cluster(&pool, old.id, "Alice")
        .await
        .unwrap()
        .expect("cluster exists");

    // Refine into two speakers. Alice's old segment [3,4] overlaps ordinal 2's turn [3,5], not
    // ordinal 1's [0,2] — so the name must follow the overlap, not the ordinal number.
    let refined = vec![
        queries::RefinedThemSegment {
            ordinal: 1,
            text: "hi".into(),
            start_s: 0.0,
            end_s: 2.0,
        },
        queries::RefinedThemSegment {
            ordinal: 2,
            text: "there".into(),
            start_s: 3.0,
            end_s: 5.0,
        },
    ];
    queries::replace_them_segments(&pool, meeting.id, &refine_result(refined))
        .await
        .unwrap();

    // Alice carried onto ordinal 2, re-bound + re-locked; ordinal 1 is a fresh unlocked speaker.
    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    let ord2 = speakers.iter().find(|s| s.ordinal == 2).unwrap();
    assert_eq!(ord2.display_name.as_deref(), Some("Alice"));
    assert!(ord2.locked);
    let ord1 = speakers.iter().find(|s| s.ordinal == 1).unwrap();
    assert_eq!(ord1.display_name, None);
    assert!(!ord1.locked);

    // The carried name labels its segment; the identity is reused, not duplicated.
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let alice_seg = segments
        .iter()
        .find(|s| s.speaker_label == "Alice")
        .expect("a segment labelled Alice");
    assert_eq!(alice_seg.text, "there");
    assert_eq!(queries::count_identities(&pool).await.unwrap(), 1);
}

#[tokio::test]
async fn replace_them_segments_empty_is_noop() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", chrono::Utc::now())
        .await
        .unwrap();
    let cluster = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "keep me",
        0.0,
        1.0,
        Some(cluster.id),
    )
    .await
    .unwrap();

    // A refine that produced nothing must leave the transcript intact (never wipe it).
    queries::replace_them_segments(&pool, meeting.id, &RefineResult::default())
        .await
        .unwrap();

    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "keep me");
    assert_eq!(
        queries::list_speaker_rows(&pool, meeting.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// A prior meeting names + locks "Alice" with a voiceprint; a later refine stores each speaker's
/// voiceprint and auto-recognizes the returning Alice (bound but unlocked — provisional).
#[tokio::test]
async fn replace_them_segments_stores_and_recognizes_voiceprints() {
    let pool = memory_pool().await;

    // Prior meeting: Alice named + locked with a stored voiceprint.
    let prior = queries::create_meeting(&pool, "prior", "p", chrono::Utc::now())
        .await
        .unwrap();
    let ac = queries::create_cluster(
        &pool,
        prior.id,
        1,
        false,
        Some(centroid_to_bytes(&[1.0, 0.0, 0.0])),
    )
    .await
    .unwrap();
    queries::rename_cluster(&pool, ac.id, "Alice")
        .await
        .unwrap()
        .expect("cluster exists");

    // New meeting: refine yields two speakers; ordinal 1's voiceprint is close to Alice's, ordinal
    // 2's is unknown.
    let meeting = queries::create_meeting(&pool, "new", "n", chrono::Utc::now())
        .await
        .unwrap();
    let result = RefineResult {
        segments: vec![
            RefinedThemSegment {
                ordinal: 1,
                text: "hey".into(),
                start_s: 0.0,
                end_s: 1.0,
            },
            RefinedThemSegment {
                ordinal: 2,
                text: "yo".into(),
                start_s: 1.0,
                end_s: 2.0,
            },
        ],
        centroids: HashMap::from([(1, vec![0.9, 0.1, 0.0]), (2, vec![0.0, 0.0, 1.0])]),
    };
    queries::replace_them_segments(&pool, meeting.id, &result)
        .await
        .unwrap();

    // Ordinal 1 recognized as Alice, bound but NOT locked (a manual rename can still override).
    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    let ord1 = speakers.iter().find(|s| s.ordinal == 1).unwrap();
    assert_eq!(ord1.display_name.as_deref(), Some("Alice"));
    assert!(!ord1.locked);
    let ord2 = speakers.iter().find(|s| s.ordinal == 2).unwrap();
    assert_eq!(ord2.display_name, None);
    assert_eq!(queries::count_identities(&pool).await.unwrap(), 1); // Alice reused

    // The recognized name labels its segment.
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert!(segments.iter().any(|s| s.speaker_label == "Alice"));

    // Both speakers' voiceprints are stored on their clusters for the next meeting.
    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT centroid FROM clusters WHERE meeting_id = ? AND ordinal = ?")
            .bind(meeting.id)
            .bind(2_i64)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(centroid_from_bytes(&stored.unwrap()), vec![0.0, 0.0, 1.0]);
}

#[tokio::test]
async fn known_voiceprints_excludes_current_and_requires_locked_centroid() {
    let pool = memory_pool().await;
    let m1 = queries::create_meeting(&pool, "m1", "1", chrono::Utc::now())
        .await
        .unwrap();
    let m2 = queries::create_meeting(&pool, "m2", "2", chrono::Utc::now())
        .await
        .unwrap();

    // m1: Alice locked + voiceprint (a candidate); Bob locked but no voiceprint (excluded).
    let alice =
        queries::create_cluster(&pool, m1.id, 1, false, Some(centroid_to_bytes(&[1.0, 0.0])))
            .await
            .unwrap();
    queries::rename_cluster(&pool, alice.id, "Alice")
        .await
        .unwrap()
        .unwrap();
    let bob = queries::create_cluster(&pool, m1.id, 2, false, None)
        .await
        .unwrap();
    queries::rename_cluster(&pool, bob.id, "Bob")
        .await
        .unwrap()
        .unwrap();

    // m2: Carol locked + voiceprint, but she is in the meeting being refined (excluded).
    let carol =
        queries::create_cluster(&pool, m2.id, 1, false, Some(centroid_to_bytes(&[0.0, 1.0])))
            .await
            .unwrap();
    queries::rename_cluster(&pool, carol.id, "Carol")
        .await
        .unwrap()
        .unwrap();

    let known = queries::known_voiceprints(&pool, m2.id).await.unwrap();
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].0, "Alice");
}
