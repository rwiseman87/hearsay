//! Schema + query round-trip tests against an in-memory SQLite database.
//!
//! A single-connection memory pool is used so every query hits the same database (each
//! connection to `sqlite::memory:` is otherwise a distinct database).

use std::collections::HashMap;

use hearsay_attribution::{centroid_from_bytes, centroid_to_bytes};
use hearsay_db::models::{MeetingStatus, Stream};
use hearsay_db::queries::{NotesResult, RefineResult, RefinedThemSegment};
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
    let meeting = queries::create_meeting(&pool, "Standup", "/tmp/standup", "", started)
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let ended = chrono::Utc::now();
    queries::finalize_meeting(&pool, meeting.id, ended, MeetingStatus::Finalized)
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
        queries::create_meeting(&pool, title, title, "", started)
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
async fn update_meeting_title_renames_and_reports_missing() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "old", "f", "", chrono::Utc::now())
        .await
        .unwrap();

    let updated = queries::update_meeting_title(&pool, meeting.id, "new title")
        .await
        .unwrap()
        .expect("meeting exists");
    assert_eq!(updated.title, "new title");
    assert!(updated.updated_at >= meeting.updated_at);
    // Persisted, not just returned.
    let fetched = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.title, "new title");

    // An unknown id renames nothing and reports `None`.
    let missing = queries::update_meeting_title(&pool, uuid::Uuid::new_v4(), "x")
        .await
        .unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn rename_cluster_binds_relabels_and_joins() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
    queries::replace_them_segments(&pool, meeting.id, &refine_result(refined), 0.6)
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
    queries::replace_them_segments(&pool, meeting.id, &refine_result(refined), 0.6)
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
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
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
    queries::replace_them_segments(&pool, meeting.id, &RefineResult::default(), 0.6)
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
    let prior = queries::create_meeting(&pool, "prior", "p", "", chrono::Utc::now())
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
    let meeting = queries::create_meeting(&pool, "new", "n", "", chrono::Utc::now())
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
    queries::replace_them_segments(&pool, meeting.id, &result, 0.6)
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
    let m1 = queries::create_meeting(&pool, "m1", "1", "", chrono::Utc::now())
        .await
        .unwrap();
    let m2 = queries::create_meeting(&pool, "m2", "2", "", chrono::Utc::now())
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

#[tokio::test]
async fn effective_settings_default_then_override() {
    let pool = memory_pool().await;

    // No preference rows -> the caller's config default is returned.
    assert!(queries::effective_record(&pool, true).await.unwrap());
    assert!(!queries::effective_record(&pool, false).await.unwrap());
    assert_eq!(
        queries::effective_output_dir(&pool, std::path::Path::new("/def"))
            .await
            .unwrap(),
        std::path::PathBuf::from("/def")
    );
    assert_eq!(
        queries::effective_speakers(&pool, true, 0.6).await.unwrap(),
        (true, 0.6)
    );

    // Stored UI overrides win over the config defaults (JSON shape matches what the settings routes
    // write for each section).
    queries::set_preference(&pool, queries::SECTION_RECORDING, r#"{"record":false}"#)
        .await
        .unwrap();
    queries::set_preference(
        &pool,
        queries::SECTION_STORAGE,
        r#"{"output_dir":"/custom/rec"}"#,
    )
    .await
    .unwrap();
    queries::set_preference(
        &pool,
        queries::SECTION_SPEAKERS,
        r#"{"auto_refine":false,"recognition_threshold":0.9}"#,
    )
    .await
    .unwrap();

    assert!(!queries::effective_record(&pool, true).await.unwrap());
    assert_eq!(
        queries::effective_output_dir(&pool, std::path::Path::new("/def"))
            .await
            .unwrap(),
        std::path::PathBuf::from("/custom/rec")
    );
    assert_eq!(
        queries::effective_speakers(&pool, true, 0.6).await.unwrap(),
        (false, 0.9)
    );
}

#[tokio::test]
async fn effective_settings_tolerate_corrupt_or_partial_rows() {
    let pool = memory_pool().await;
    // A non-JSON row falls back to the default rather than erroring.
    queries::set_preference(&pool, queries::SECTION_RECORDING, "not json")
        .await
        .unwrap();
    assert!(queries::effective_record(&pool, true).await.unwrap());
    // A partial speakers row keeps the missing field's default (each field resolves independently).
    queries::set_preference(&pool, queries::SECTION_SPEAKERS, r#"{"auto_refine":false}"#)
        .await
        .unwrap();
    assert_eq!(
        queries::effective_speakers(&pool, true, 0.55)
            .await
            .unwrap(),
        (false, 0.55)
    );
}

#[tokio::test]
async fn meeting_dir_pins_at_creation_with_legacy_fallback() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "M", "2026-07-14_0900_m", "", chrono::Utc::now())
        .await
        .unwrap();
    // A freshly created row has no pinned dir yet; dir_path falls back to default_root/<folder>.
    assert_eq!(meeting.dir, "");
    assert_eq!(
        meeting.dir_path(std::path::Path::new("/root")),
        std::path::PathBuf::from("/root/2026-07-14_0900_m")
    );

    // Once pinned, dir_path returns the pinned absolute dir regardless of the current default root.
    queries::set_meeting_dir(&pool, meeting.id, "/custom/2026-07-14_0900_m")
        .await
        .unwrap();
    let pinned = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pinned.dir, "/custom/2026-07-14_0900_m");
    assert_eq!(
        pinned.dir_path(std::path::Path::new("/root")),
        std::path::PathBuf::from("/custom/2026-07-14_0900_m")
    );
}

#[tokio::test]
async fn recognition_threshold_gates_cross_meeting_match() {
    let pool = memory_pool().await;
    // Prior meeting: Alice named + locked with a stored voiceprint.
    let prior = queries::create_meeting(&pool, "prior", "p", "", chrono::Utc::now())
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

    // New meeting whose ordinal-1 voiceprint is ~0.994 cosine to Alice: recognized at 0.6, but a
    // stricter 0.999 threshold rejects the same match -> the threshold is genuinely applied.
    let meeting = queries::create_meeting(&pool, "new", "n", "", chrono::Utc::now())
        .await
        .unwrap();
    let result = RefineResult {
        segments: vec![RefinedThemSegment {
            ordinal: 1,
            text: "hi".into(),
            start_s: 0.0,
            end_s: 1.0,
        }],
        centroids: HashMap::from([(1, vec![0.9, 0.1, 0.0])]),
    };
    queries::replace_them_segments(&pool, meeting.id, &result, 0.999)
        .await
        .unwrap();

    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    let ord1 = speakers.iter().find(|s| s.ordinal == 1).unwrap();
    assert_eq!(
        ord1.display_name, None,
        "0.999 threshold must reject ~0.994"
    );
}

#[tokio::test]
async fn meeting_notes_upsert_get_and_regenerate() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "Sync", "/tmp/sync", "", chrono::Utc::now())
        .await
        .unwrap();

    let first = NotesResult {
        summary: "We discussed the roadmap.".into(),
        action_items: vec!["Ship the beta".into(), "Email the client".into()],
    };
    queries::upsert_meeting_notes(&pool, meeting.id, &first, "qwen3-4b")
        .await
        .unwrap();

    let stored = queries::get_meeting_notes(&pool, meeting.id)
        .await
        .unwrap()
        .expect("notes exist");
    assert_eq!(stored.summary, first.summary);
    assert_eq!(stored.model, "qwen3-4b");
    let items: Vec<String> = serde_json::from_str(&stored.action_items).unwrap();
    assert_eq!(items, first.action_items);
    let created = stored.created_at;

    // Regenerating overwrites summary/items/model in place (still one row) and preserves created_at.
    let second = NotesResult {
        summary: "Revised summary.".into(),
        action_items: vec!["One item".into()],
    };
    queries::upsert_meeting_notes(&pool, meeting.id, &second, "smollm3-3b")
        .await
        .unwrap();
    let stored = queries::get_meeting_notes(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.summary, "Revised summary.");
    assert_eq!(stored.model, "smollm3-3b");
    assert_eq!(stored.created_at, created, "created_at preserved on upsert");
    let items: Vec<String> = serde_json::from_str(&stored.action_items).unwrap();
    assert_eq!(items, vec!["One item".to_string()]);
}

#[tokio::test]
async fn delete_meeting_cascades_notes() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "Sync", "/tmp/sync", "", chrono::Utc::now())
        .await
        .unwrap();
    queries::upsert_meeting_notes(
        &pool,
        meeting.id,
        &NotesResult {
            summary: "s".into(),
            action_items: vec![],
        },
        "m",
    )
    .await
    .unwrap();

    assert!(queries::delete_meeting(&pool, meeting.id).await.unwrap());
    assert!(
        queries::get_meeting_notes(&pool, meeting.id)
            .await
            .unwrap()
            .is_none(),
        "notes cascade-deleted with the meeting"
    );
}

#[tokio::test]
async fn folder_crud_roundtrip_and_reports_missing() {
    let pool = memory_pool().await;
    let work = queries::create_folder(&pool, "Work", None).await.unwrap();
    assert_eq!(work.parent_id, None);
    let project = queries::create_folder(&pool, "Project", Some(work.id))
        .await
        .unwrap();
    assert_eq!(project.parent_id, Some(work.id));

    assert_eq!(queries::count_folders(&pool).await.unwrap(), 2);
    // Ordered by name (case-insensitive): "Project" before "Work".
    let listed = queries::list_folders(&pool, 50, 0).await.unwrap();
    assert_eq!(
        listed.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        vec!["Project", "Work"]
    );

    let renamed = queries::update_folder_name(&pool, work.id, "Work stuff")
        .await
        .unwrap()
        .expect("existing folder renamed");
    assert_eq!(renamed.name, "Work stuff");
    assert!(renamed.updated_at >= work.updated_at);

    assert!(
        queries::update_folder_name(&pool, uuid::Uuid::new_v4(), "x")
            .await
            .unwrap()
            .is_none(),
        "renaming a missing folder reports None"
    );
}

#[tokio::test]
async fn reparent_folder_and_descendant_cycle_guard() {
    let pool = memory_pool().await;
    let a = queries::create_folder(&pool, "A", None).await.unwrap();
    let b = queries::create_folder(&pool, "B", None).await.unwrap();

    // Move B under A, then C under B: A > B > C.
    let moved = queries::set_folder_parent(&pool, b.id, Some(a.id))
        .await
        .unwrap()
        .expect("existing folder reparented");
    assert_eq!(moved.parent_id, Some(a.id));
    let c = queries::create_folder(&pool, "C", Some(b.id))
        .await
        .unwrap();

    // A descendant is itself, a child, or a deeper node; a sibling/ancestor is not.
    assert!(queries::folder_is_descendant(&pool, a.id, a.id)
        .await
        .unwrap());
    assert!(queries::folder_is_descendant(&pool, b.id, a.id)
        .await
        .unwrap());
    assert!(queries::folder_is_descendant(&pool, c.id, a.id)
        .await
        .unwrap());
    assert!(!queries::folder_is_descendant(&pool, a.id, b.id)
        .await
        .unwrap());

    // Detaching to the root is allowed.
    let detached = queries::set_folder_parent(&pool, b.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detached.parent_id, None);
    assert!(
        queries::set_folder_parent(&pool, uuid::Uuid::new_v4(), None)
            .await
            .unwrap()
            .is_none(),
        "reparenting a missing folder reports None"
    );
}

#[tokio::test]
async fn assign_meeting_folder_sets_and_clears() {
    let pool = memory_pool().await;
    let folder = queries::create_folder(&pool, "Clients", None)
        .await
        .unwrap();
    let meeting = queries::create_meeting(&pool, "Kickoff", "/tmp/kickoff", "", chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(meeting.folder_id, None);

    let filed = queries::assign_meeting_folder(&pool, meeting.id, Some(folder.id))
        .await
        .unwrap()
        .expect("existing meeting filed");
    assert_eq!(filed.folder_id, Some(folder.id));

    let unfiled = queries::assign_meeting_folder(&pool, meeting.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unfiled.folder_id, None);

    assert!(
        queries::assign_meeting_folder(&pool, uuid::Uuid::new_v4(), Some(folder.id))
            .await
            .unwrap()
            .is_none(),
        "filing a missing meeting reports None"
    );
}

#[tokio::test]
async fn delete_folder_cascades_subtree_and_unfiles_meetings() {
    let pool = memory_pool().await;
    // A > B, with a meeting filed in each level.
    let a = queries::create_folder(&pool, "A", None).await.unwrap();
    let b = queries::create_folder(&pool, "B", Some(a.id))
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let m_a = queries::create_meeting(&pool, "in A", "/tmp/a", "", now)
        .await
        .unwrap();
    let m_b = queries::create_meeting(&pool, "in B", "/tmp/b", "", now)
        .await
        .unwrap();
    queries::assign_meeting_folder(&pool, m_a.id, Some(a.id))
        .await
        .unwrap();
    queries::assign_meeting_folder(&pool, m_b.id, Some(b.id))
        .await
        .unwrap();

    assert!(queries::delete_folder(&pool, a.id).await.unwrap());

    // The whole folder subtree is gone (ON DELETE CASCADE)...
    assert!(queries::get_folder(&pool, a.id).await.unwrap().is_none());
    assert!(
        queries::get_folder(&pool, b.id).await.unwrap().is_none(),
        "the sub-folder cascade-deletes with its parent"
    );
    // ...but the meetings survive, merely un-filed (ON DELETE SET NULL).
    assert_eq!(
        queries::get_meeting(&pool, m_a.id)
            .await
            .unwrap()
            .unwrap()
            .folder_id,
        None
    );
    assert_eq!(
        queries::get_meeting(&pool, m_b.id)
            .await
            .unwrap()
            .unwrap()
            .folder_id,
        None,
        "a meeting in the deleted subtree is un-filed, not deleted"
    );

    assert!(
        !queries::delete_folder(&pool, uuid::Uuid::new_v4())
            .await
            .unwrap(),
        "deleting a missing folder reports false"
    );
}
