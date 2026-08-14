//! Schema + query round-trip tests against an in-memory SQLite database.
//!
//! A single-connection memory pool is used so every query hits the same database (each
//! connection to `sqlite::memory:` is otherwise a distinct database).

use std::borrow::Cow;
use std::collections::HashMap;

use chrono::Utc;
use hearsay_attribution::{centroid_from_bytes, centroid_to_bytes};
use hearsay_db::models::{MeetingStatus, Stream};
use hearsay_db::queries;
use hearsay_db::queries::{NotesResult, RefineResult, RefinedThemSegment};
use hearsay_db::test_support::memory_pool;
use hearsay_db::{connect_options, MIGRATOR};
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

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

    let renamed = queries::rename_cluster(&pool, cluster.meeting_id, cluster.id, "  Zed  ")
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

    assert!(
        queries::rename_cluster(&pool, cluster.meeting_id, uuid::Uuid::new_v4(), "X")
            .await
            .unwrap()
            .is_none()
    );
}

/// A named + locked cluster carrying a centroid in its own meeting — one stored voiceprint.
async fn seed_voiceprint(pool: &SqlitePool, meeting: &str, person: &str, dims: usize) {
    let m = queries::create_meeting(pool, meeting, meeting, "", chrono::Utc::now())
        .await
        .unwrap();
    let centroid = centroid_to_bytes(&vec![0.5f32; dims]);
    let cluster = queries::create_cluster(pool, m.id, 1, false, Some(centroid))
        .await
        .unwrap();
    queries::rename_cluster(pool, m.id, cluster.id, person)
        .await
        .unwrap();
}

#[tokio::test]
async fn list_voiceprints_covers_only_people_with_an_embedding() {
    let pool = memory_pool().await;
    seed_voiceprint(&pool, "one", "Alice", 4).await;
    seed_voiceprint(&pool, "two", "Alice", 4).await;
    seed_voiceprint(&pool, "three", "Bob", 8).await;
    // Named, but the meeting was never refined: an identity with no embedding.
    let bare = queries::create_meeting(&pool, "bare", "bare", "", chrono::Utc::now())
        .await
        .unwrap();
    let unbound = queries::create_cluster(&pool, bare.id, 1, false, None)
        .await
        .unwrap();
    queries::rename_cluster(&pool, bare.id, unbound.id, "Carol")
        .await
        .unwrap();

    let people = queries::list_voiceprints(&pool, 50, 0).await.unwrap();
    let names: Vec<&str> = people.iter().map(|p| p.display_name.as_str()).collect();
    assert!(
        !names.contains(&"Carol"),
        "no embedding, no roster row: {names:?}"
    );
    assert_eq!(
        queries::count_voiceprint_people(&pool).await.unwrap(),
        people.len() as i64,
        "the envelope total must describe the rows the list returns"
    );

    let alice = people.iter().find(|p| p.display_name == "Alice").unwrap();
    assert_eq!(alice.samples.len(), 2);
    // Newest meeting first, so the caller can take `samples[0]` as "last heard".
    assert!(alice.samples[0].started_at >= alice.samples[1].started_at);
    // The dimension is derived from the blob length, never by decoding it.
    assert!(alice.samples.iter().all(|s| s.dimension == 4));
    assert!(alice.samples.iter().all(|s| s.locked));
    let bob = people.iter().find(|p| p.display_name == "Bob").unwrap();
    assert_eq!(bob.samples[0].dimension, 8);
}

#[tokio::test]
async fn list_voiceprints_pages_over_people_not_samples() {
    let pool = memory_pool().await;
    for person in ["Alice", "Bob", "Carol"] {
        seed_voiceprint(&pool, &format!("{person}-1"), person, 4).await;
        seed_voiceprint(&pool, &format!("{person}-2"), person, 4).await;
    }
    assert_eq!(queries::count_voiceprint_people(&pool).await.unwrap(), 3);

    // A page of 2 must be two *people* with both their samples, not two sample rows.
    let page = queries::list_voiceprints(&pool, 2, 0).await.unwrap();
    assert_eq!(page.len(), 2);
    assert!(page.iter().all(|p| p.samples.len() == 2));

    let rest = queries::list_voiceprints(&pool, 2, 2).await.unwrap();
    assert_eq!(rest.len(), 1);
    // No one appears on both pages.
    for person in &page {
        assert_ne!(person.identity_id, rest[0].identity_id);
    }
}

#[tokio::test]
async fn paging_people_never_repeats_or_drops_anyone() {
    let pool = memory_pool().await;
    // Enough people, created back to back, that any instability in the ordering shows up. They are
    // created in a tight loop, so several share an `updated_at` to the microsecond — which is
    // exactly the case that makes ordering by it alone undefined.
    let names: Vec<String> = (0..7).map(|i| format!("Person {i}")).collect();
    for name in &names {
        // Two samples each: the roster's join fans out per sample, and it is that shape — not a
        // one-row-per-person one — that exposes an unstable page window.
        seed_voiceprint(&pool, &format!("{name}-a"), name, 4).await;
        seed_voiceprint(&pool, &format!("{name}-b"), name, 4).await;
    }

    // Walk both rosters two at a time and confirm the union is every person, exactly once. Ordering
    // by a non-unique column without a tiebreaker silently repeats one row and loses another here.
    let mut seen_people = Vec::new();
    let mut seen_identities = Vec::new();
    for offset in (0..8).step_by(2) {
        for person in queries::list_voiceprints(&pool, 2, offset).await.unwrap() {
            seen_people.push(person.display_name);
        }
        for identity in queries::list_identities(&pool, 2, offset).await.unwrap() {
            seen_identities.push(identity.display_name);
        }
    }
    for (label, mut seen) in [
        ("voiceprints", seen_people),
        ("identities", seen_identities),
    ] {
        let total = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), total, "{label}: a person appeared on two pages");
        assert_eq!(
            seen, names,
            "{label}: paging did not cover everyone exactly once"
        );
    }
}

#[tokio::test]
async fn rename_identity_reports_conflicts_and_leaves_the_row_alone() {
    let pool = memory_pool().await;
    seed_voiceprint(&pool, "one", "Alice", 4).await;
    seed_voiceprint(&pool, "two", "Bob", 4).await;
    let people = queries::list_voiceprints(&pool, 50, 0).await.unwrap();
    let alice = people
        .iter()
        .find(|p| p.display_name == "Alice")
        .unwrap()
        .identity_id;

    // Taking a name someone else holds is refused rather than silently folding the two together.
    assert_eq!(
        queries::rename_identity(&pool, alice, "Bob").await.unwrap(),
        queries::RenameIdentityOutcome::NameTaken
    );
    assert_eq!(
        queries::rename_identity(&pool, uuid::Uuid::new_v4(), "Zed")
            .await
            .unwrap(),
        queries::RenameIdentityOutcome::NotFound
    );

    // Re-applying the current name writes nothing, so there is nothing to re-export.
    match queries::rename_identity(&pool, alice, "Alice")
        .await
        .unwrap()
    {
        queries::RenameIdentityOutcome::Renamed { meeting_ids, .. } => {
            assert!(meeting_ids.is_empty(), "a no-op rename touches no meeting")
        }
        other => panic!("expected Renamed, got {other:?}"),
    }

    // The refused rename left both names intact.
    let after = queries::list_voiceprints(&pool, 50, 0).await.unwrap();
    let mut names: Vec<&str> = after.iter().map(|p| p.display_name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Alice", "Bob"]);
}

#[tokio::test]
async fn forgetting_a_voice_clears_unlocked_centroids_too() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "m", "m", "", chrono::Utc::now())
        .await
        .unwrap();
    let centroid = centroid_to_bytes(&[0.6, 0.8]);
    let locked = queries::create_cluster(&pool, meeting.id, 1, false, Some(centroid.clone()))
        .await
        .unwrap();
    queries::rename_cluster(&pool, meeting.id, locked.id, "Alice")
        .await
        .unwrap();
    let identity = queries::list_speaker_rows(&pool, meeting.id).await.unwrap()[0]
        .identity_id
        .unwrap();
    // A second meeting where Alice was auto-recognized: bound to her, centroid stored, but never
    // confirmed by hand so `locked` is 0.
    let other = queries::create_meeting(&pool, "o", "o", "", chrono::Utc::now())
        .await
        .unwrap();
    let recognized = queries::create_cluster(&pool, other.id, 1, false, Some(centroid))
        .await
        .unwrap();
    sqlx::query("UPDATE clusters SET identity_id = ? WHERE id = ?")
        .bind(identity)
        .bind(recognized.id)
        .execute(&pool)
        .await
        .unwrap();

    assert!(queries::forget_identity_voice(&pool, identity)
        .await
        .unwrap());

    // Both are cleared. Leaving the unlocked one would let it re-enter the candidate set the moment
    // anyone renamed that cluster, quietly undoing the "forget".
    let remaining: Vec<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT centroid FROM clusters WHERE identity_id = ?")
            .bind(identity)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().all(|c| c.is_none()));
    // She is gone from the roster but still a known person labelling her past lines.
    assert!(queries::list_voiceprints(&pool, 50, 0)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(queries::count_identities(&pool).await.unwrap(), 1);
}

#[tokio::test]
async fn clearing_one_centroid_is_idempotent() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "m", "m", "", chrono::Utc::now())
        .await
        .unwrap();
    let cluster = queries::create_cluster(
        &pool,
        meeting.id,
        1,
        true,
        Some(centroid_to_bytes(&[1.0, 0.0])),
    )
    .await
    .unwrap();

    // Repeating the call stays a success: the route maps `false` to a 404, and a second delete of
    // something already gone should not look like a missing resource.
    assert!(queries::clear_cluster_centroid(&pool, cluster.id)
        .await
        .unwrap());
    assert!(queries::clear_cluster_centroid(&pool, cluster.id)
        .await
        .unwrap());
    assert!(
        !queries::clear_cluster_centroid(&pool, uuid::Uuid::new_v4())
            .await
            .unwrap()
    );

    // The cluster itself, and its lock, survive.
    let rows = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].locked);
}

#[tokio::test]
async fn merge_clusters_repoints_lines_and_drops_the_source() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let source = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    let target = queries::create_cluster(&pool, meeting.id, 2, false, None)
        .await
        .unwrap();
    queries::rename_cluster(&pool, meeting.id, target.id, "Alice")
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "alpha",
        0.0,
        1.0,
        Some(source.id),
    )
    .await
    .unwrap();

    assert_eq!(
        queries::merge_clusters(&pool, meeting.id, source.id, target.id)
            .await
            .unwrap(),
        queries::MergeOutcome::Merged
    );

    // The line survives on the target — `segments.cluster_id` is ON DELETE SET NULL, so a merge that
    // dropped the cluster before repointing would silently orphan it instead.
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].cluster_id, Some(target.id));
    assert_eq!(segments[0].speaker_label, "Alice");
    assert!(segments[0].edited);

    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    assert_eq!(speakers.len(), 1);
    assert_eq!(speakers[0].id, target.id);
    assert_eq!(speakers[0].ordinal, 2, "the source's ordinal is not reused");
}

#[tokio::test]
async fn merge_clusters_reports_bad_targets_without_touching_anything() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let other = queries::create_meeting(&pool, "o", "o", "", chrono::Utc::now())
        .await
        .unwrap();
    let a = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    let b = queries::create_cluster(&pool, meeting.id, 2, false, None)
        .await
        .unwrap();
    let foreign = queries::create_cluster(&pool, other.id, 1, false, None)
        .await
        .unwrap();

    let cases = [
        (a.id, a.id, queries::MergeOutcome::SameCluster),
        (
            uuid::Uuid::new_v4(),
            b.id,
            queries::MergeOutcome::SourceNotFound,
        ),
        (foreign.id, b.id, queries::MergeOutcome::SourceNotFound),
        (
            a.id,
            uuid::Uuid::new_v4(),
            queries::MergeOutcome::TargetNotFound,
        ),
        (a.id, foreign.id, queries::MergeOutcome::TargetNotFound),
    ];
    for (source, target, want) in cases {
        assert_eq!(
            queries::merge_clusters(&pool, meeting.id, source, target)
                .await
                .unwrap(),
            want,
            "{source} -> {target}"
        );
    }
    assert_eq!(
        queries::list_speaker_rows(&pool, meeting.id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        queries::list_speaker_rows(&pool, other.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn rename_cluster_is_scoped_to_its_meeting() {
    let pool = memory_pool().await;
    let mine = queries::create_meeting(&pool, "mine", "m", "", chrono::Utc::now())
        .await
        .unwrap();
    let theirs = queries::create_meeting(&pool, "theirs", "t", "", chrono::Utc::now())
        .await
        .unwrap();
    let cluster = queries::create_cluster(&pool, theirs.id, 1, false, None)
        .await
        .unwrap();

    // A cluster id from another meeting must not be renameable through this meeting's route.
    assert!(queries::rename_cluster(&pool, mine.id, cluster.id, "Alice")
        .await
        .unwrap()
        .is_none());
    let speakers = queries::list_speaker_rows(&pool, theirs.id).await.unwrap();
    assert_eq!(speakers[0].display_name, None);
}

#[tokio::test]
async fn reassign_segment_speaker_moves_line_and_creates_named_speaker() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "t", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let c1 = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    let c2 = queries::create_cluster(&pool, meeting.id, 2, false, None)
        .await
        .unwrap();
    let seg_a = queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "alpha",
        0.0,
        1.0,
        Some(c1.id),
    )
    .await
    .unwrap();
    let seg_b = queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "beta",
        1.0,
        2.0,
        Some(c1.id),
    )
    .await
    .unwrap();
    let me_seg =
        queries::insert_segment(&pool, meeting.id, Stream::Me, "Me", "mine", 2.0, 3.0, None)
            .await
            .unwrap();

    // Move seg_a to the existing (unbound) cluster c2 — label falls back to its ordinal.
    let updated = match queries::reassign_segment_speaker(
        &pool,
        meeting.id,
        seg_a.id,
        queries::SpeakerTarget::Cluster(c2.id),
    )
    .await
    .unwrap()
    {
        queries::ReassignOutcome::Reassigned(s) => s,
        other => panic!("expected reassigned, got {other:?}"),
    };
    assert_eq!(updated.cluster_id, Some(c2.id));
    assert_eq!(updated.speaker_label, "Speaker 2");
    assert!(updated.edited);
    // seg_b is left alone (only the one line moved).
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let b = segments.iter().find(|s| s.id == seg_b.id).unwrap();
    assert_eq!(b.cluster_id, Some(c1.id));
    assert!(!b.edited);

    // Assign seg_a to a new named speaker: a fresh locked cluster + identity.
    let dana_a = match queries::reassign_segment_speaker(
        &pool,
        meeting.id,
        seg_a.id,
        queries::SpeakerTarget::Name("Dana"),
    )
    .await
    .unwrap()
    {
        queries::ReassignOutcome::Reassigned(s) => s,
        other => panic!("expected reassigned, got {other:?}"),
    };
    assert_eq!(dana_a.speaker_label, "Dana");
    let dana_cluster = dana_a.cluster_id.expect("bound to a cluster");
    assert_ne!(dana_cluster, c1.id);
    assert_ne!(dana_cluster, c2.id);

    // A second line given the same name reuses that person's cluster (one identity, one color).
    let dana_b = match queries::reassign_segment_speaker(
        &pool,
        meeting.id,
        seg_b.id,
        queries::SpeakerTarget::Name("Dana"),
    )
    .await
    .unwrap()
    {
        queries::ReassignOutcome::Reassigned(s) => s,
        other => panic!("expected reassigned, got {other:?}"),
    };
    assert_eq!(dana_b.cluster_id, Some(dana_cluster));
    assert_eq!(queries::count_identities(&pool).await.unwrap(), 1);
    let rows = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    let dana_row = rows.iter().find(|r| r.id == dana_cluster).unwrap();
    assert_eq!(dana_row.display_name.as_deref(), Some("Dana"));
    assert!(dana_row.locked);

    // Reassigning to a cluster that is *bound* to an identity resolves the label from that identity
    // (not the ordinal). Bind c2 to "Cara", then move seg_a onto c2.
    queries::rename_cluster(&pool, c2.meeting_id, c2.id, "Cara")
        .await
        .unwrap()
        .expect("cluster exists");
    let cara = match queries::reassign_segment_speaker(
        &pool,
        meeting.id,
        seg_a.id,
        queries::SpeakerTarget::Cluster(c2.id),
    )
    .await
    .unwrap()
    {
        queries::ReassignOutcome::Reassigned(s) => s,
        other => panic!("expected reassigned, got {other:?}"),
    };
    assert_eq!(cara.cluster_id, Some(c2.id));
    assert_eq!(cara.speaker_label, "Cara");

    // A `Name` target for an identity that already exists but has no cluster in this meeting reuses
    // the identity (no duplicate) and creates a fresh cluster bound to it.
    queries::create_identity(&pool, "Evan", None).await.unwrap();
    let before = queries::count_identities(&pool).await.unwrap(); // Dana + Cara + Evan
    let evan = match queries::reassign_segment_speaker(
        &pool,
        meeting.id,
        seg_b.id,
        queries::SpeakerTarget::Name("Evan"),
    )
    .await
    .unwrap()
    {
        queries::ReassignOutcome::Reassigned(s) => s,
        other => panic!("expected reassigned, got {other:?}"),
    };
    assert_eq!(evan.speaker_label, "Evan");
    let evan_cluster = evan.cluster_id.expect("bound to a cluster");
    assert_ne!(evan_cluster, dana_cluster);
    assert_ne!(evan_cluster, c2.id);
    assert_eq!(queries::count_identities(&pool).await.unwrap(), before); // reused, not duplicated

    // Guards: unknown segment, a Me line, and a target cluster from nowhere.
    assert_eq!(
        queries::reassign_segment_speaker(
            &pool,
            meeting.id,
            uuid::Uuid::new_v4(),
            queries::SpeakerTarget::Cluster(c2.id)
        )
        .await
        .unwrap(),
        queries::ReassignOutcome::SegmentNotFound
    );
    assert_eq!(
        queries::reassign_segment_speaker(
            &pool,
            meeting.id,
            me_seg.id,
            queries::SpeakerTarget::Cluster(c2.id)
        )
        .await
        .unwrap(),
        queries::ReassignOutcome::NotThemStream
    );
    assert_eq!(
        queries::reassign_segment_speaker(
            &pool,
            meeting.id,
            seg_a.id,
            queries::SpeakerTarget::Cluster(uuid::Uuid::new_v4())
        )
        .await
        .unwrap(),
        queries::ReassignOutcome::ClusterNotFound
    );
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
    queries::rename_cluster(&pool, old.meeting_id, old.id, "Alice")
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
    queries::rename_cluster(&pool, ac.meeting_id, ac.id, "Alice")
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
    queries::rename_cluster(&pool, alice.meeting_id, alice.id, "Alice")
        .await
        .unwrap()
        .unwrap();
    let bob = queries::create_cluster(&pool, m1.id, 2, false, None)
        .await
        .unwrap();
    queries::rename_cluster(&pool, bob.meeting_id, bob.id, "Bob")
        .await
        .unwrap()
        .unwrap();

    // m2: Carol locked + voiceprint, but she is in the meeting being refined (excluded).
    let carol =
        queries::create_cluster(&pool, m2.id, 1, false, Some(centroid_to_bytes(&[0.0, 1.0])))
            .await
            .unwrap();
    queries::rename_cluster(&pool, carol.meeting_id, carol.id, "Carol")
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
async fn effective_compression_defaults_and_overrides() {
    let pool = memory_pool().await;

    // No stored row: the config defaults stand.
    assert_eq!(
        queries::effective_compression(&pool, true, 7)
            .await
            .unwrap(),
        (true, 7)
    );

    // A storage row written before archival existed carries only `output_dir`. Each field must fall
    // back independently — otherwise the feature would arrive disabled (or at 0 days) on every
    // install that had ever set a recordings folder.
    queries::set_preference(
        &pool,
        queries::SECTION_STORAGE,
        r#"{"output_dir":"/custom/rec"}"#,
    )
    .await
    .unwrap();
    assert_eq!(
        queries::effective_compression(&pool, true, 7)
            .await
            .unwrap(),
        (true, 7)
    );

    // A full row wins.
    queries::set_preference(
        &pool,
        queries::SECTION_STORAGE,
        r#"{"output_dir":"/custom/rec","compress_audio":false,"compress_after_days":30}"#,
    )
    .await
    .unwrap();
    assert_eq!(
        queries::effective_compression(&pool, true, 7)
            .await
            .unwrap(),
        (false, 30)
    );

    // A corrupt row degrades to the defaults rather than failing the sweep.
    queries::set_preference(&pool, queries::SECTION_STORAGE, "not json")
        .await
        .unwrap();
    assert_eq!(
        queries::effective_compression(&pool, false, 14)
            .await
            .unwrap(),
        (false, 14)
    );
}

#[tokio::test]
async fn list_finalized_before_selects_only_aged_finalized_meetings() {
    let pool = memory_pool().await;
    let now = Utc::now();
    let old = now - chrono::Duration::days(30);

    // Finalized and old: eligible.
    let aged = queries::create_meeting(&pool, "aged", "aged", "/tmp/aged", old)
        .await
        .unwrap();
    queries::finalize_meeting(&pool, aged.id, old, MeetingStatus::Finalized)
        .await
        .unwrap();

    // Finalized but recent: not yet.
    let fresh = queries::create_meeting(&pool, "fresh", "fresh", "/tmp/fresh", now)
        .await
        .unwrap();
    queries::finalize_meeting(&pool, fresh.id, now, MeetingStatus::Finalized)
        .await
        .unwrap();

    // Old but still recording: never a candidate, however old it looks.
    let live = queries::create_meeting(&pool, "live", "live", "/tmp/live", old)
        .await
        .unwrap();

    // Finalized by the startup reconcile after a hard exit, so `ended_at` was never stamped: the
    // age must fall back to `started_at`, or this meeting is archived never.
    let no_end = queries::create_meeting(&pool, "no-end", "no-end", "/tmp/no-end", old)
        .await
        .unwrap();
    queries::set_meeting_finalized(&pool, no_end.id)
        .await
        .unwrap();

    let cutoff = now - chrono::Duration::days(7);
    let found = queries::list_finalized_before(&pool, cutoff).await.unwrap();
    let ids: Vec<_> = found.iter().map(|m| m.id).collect();

    assert!(ids.contains(&aged.id), "aged meeting should be eligible");
    assert!(
        ids.contains(&no_end.id),
        "null ended_at should use started_at"
    );
    assert!(!ids.contains(&fresh.id), "recent meeting should be skipped");
    assert!(
        !ids.contains(&live.id),
        "recording meeting should be skipped"
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
    queries::rename_cluster(&pool, ac.meeting_id, ac.id, "Alice")
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
        content: "## Summary\nWe discussed the roadmap.\n\n## Action items\n- Ship the beta".into(),
    };
    queries::upsert_meeting_notes(&pool, meeting.id, &first, "qwen3-4b")
        .await
        .unwrap();

    let stored = queries::get_meeting_notes(&pool, meeting.id)
        .await
        .unwrap()
        .expect("notes exist");
    assert_eq!(stored.content, first.content);
    assert_eq!(stored.model, "qwen3-4b");
    let created = stored.created_at;

    // Regenerating overwrites content/model in place (still one row) and preserves created_at.
    let second = NotesResult {
        content: "Revised notes.".into(),
    };
    queries::upsert_meeting_notes(&pool, meeting.id, &second, "smollm3-3b")
        .await
        .unwrap();
    let stored = queries::get_meeting_notes(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.content, "Revised notes.");
    assert_eq!(stored.model, "smollm3-3b");
    assert_eq!(stored.created_at, created, "created_at preserved on upsert");
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
            content: "s".into(),
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
async fn user_notes_upsert_get_overwrite_and_cascade() {
    let pool = memory_pool().await;
    let meeting = queries::create_meeting(&pool, "Discovery", "/tmp/disc", "", chrono::Utc::now())
        .await
        .unwrap();

    // No row yet is the empty state.
    assert!(queries::get_user_notes(&pool, meeting.id)
        .await
        .unwrap()
        .is_none());

    let first = queries::upsert_user_notes(&pool, meeting.id, "renewals are manual")
        .await
        .unwrap();
    assert_eq!(first.body, "renewals are manual");
    let created = first.created_at;

    // A later save overwrites the body in place (still one row) and preserves created_at.
    let second = queries::upsert_user_notes(&pool, meeting.id, "renewals are manual\nask budget")
        .await
        .unwrap();
    assert_eq!(second.body, "renewals are manual\nask budget");
    assert_eq!(
        second.created_at, created,
        "created_at preserved on autosave"
    );

    let stored = queries::get_user_notes(&pool, meeting.id)
        .await
        .unwrap()
        .expect("notes exist");
    assert_eq!(stored.body, "renewals are manual\nask budget");

    assert!(queries::delete_meeting(&pool, meeting.id).await.unwrap());
    assert!(
        queries::get_user_notes(&pool, meeting.id)
            .await
            .unwrap()
            .is_none(),
        "user notes cascade-deleted with the meeting"
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

#[tokio::test]
async fn search_ranks_hits_with_highlight_snippet_and_ignores_nonmatches() {
    let pool = memory_pool().await;
    let started = chrono::Utc::now();
    let review = queries::create_meeting(&pool, "Review", "/tmp/review", "", started)
        .await
        .unwrap();
    let lunch = queries::create_meeting(&pool, "Lunch", "/tmp/lunch", "", started)
        .await
        .unwrap();
    // Two segments mention "budget" (across two meetings); one does not. The FTS index is kept in
    // sync by the segments_ai trigger, so a plain insert is immediately searchable.
    queries::insert_segment(
        &pool,
        review.id,
        Stream::Them,
        "Speaker 1",
        "the quarterly budget review",
        0.0,
        1.0,
        None,
    )
    .await
    .unwrap();
    queries::insert_segment(
        &pool,
        lunch.id,
        Stream::Me,
        "Me",
        "budget forecast for next year",
        0.0,
        1.0,
        None,
    )
    .await
    .unwrap();
    queries::insert_segment(
        &pool,
        lunch.id,
        Stream::Me,
        "Me",
        "lunch plans on friday",
        1.0,
        2.0,
        None,
    )
    .await
    .unwrap();

    // A prefix token is exactly what routes::search::build_match emits ("budget" -> "budget*").
    assert_eq!(queries::count_search(&pool, "budget*").await.unwrap(), 2);
    assert_eq!(queries::count_search(&pool, "zznope*").await.unwrap(), 0);

    let hits = queries::search_segments(&pool, "budget*", 10, 0)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    // Each hit carries its meeting context (both meetings match) ...
    let titles: std::collections::HashSet<&str> =
        hits.iter().map(|h| h.meeting_title.as_str()).collect();
    assert!(titles.contains("Review") && titles.contains("Lunch"));
    // ... and a snippet that wraps the match in the private-use highlight sentinels U+E000/U+E001
    // (char(57344)/char(57345)) — no HTML in the payload.
    let hit = hits
        .iter()
        .find(|h| h.meeting_title == "Review")
        .expect("the Review meeting matched");
    assert!(
        hit.snippet.contains('\u{E000}') && hit.snippet.contains('\u{E001}'),
        "snippet highlights the match with sentinels: {:?}",
        hit.snippet
    );
    assert!(hit.snippet.to_lowercase().contains("budget"));
}

#[tokio::test]
async fn update_segment_text_scopes_to_its_meeting_and_reports_missing() {
    let pool = memory_pool().await;
    let started = chrono::Utc::now();
    let a = queries::create_meeting(&pool, "A", "/tmp/a", "", started)
        .await
        .unwrap();
    let b = queries::create_meeting(&pool, "B", "/tmp/b", "", started)
        .await
        .unwrap();
    let seg = queries::insert_segment(
        &pool,
        a.id,
        Stream::Them,
        "Speaker 1",
        "original",
        0.0,
        1.0,
        None,
    )
    .await
    .unwrap();
    assert!(!seg.edited);

    // Editing via the owning meeting updates the text and marks the row edited.
    let edited = queries::update_segment_text(&pool, a.id, seg.id, "corrected")
        .await
        .unwrap()
        .expect("segment edited via its own meeting");
    assert_eq!(edited.text, "corrected");
    assert!(edited.edited, "a manual edit sets the edited flag");

    // A segment id from another meeting cannot be edited via B's route (the `AND meeting_id` scope
    // is a cross-meeting boundary): None, and the row is untouched.
    assert!(
        queries::update_segment_text(&pool, b.id, seg.id, "hijacked")
            .await
            .unwrap()
            .is_none(),
        "a segment cannot be edited through a different meeting's id"
    );
    // A segment id that does not exist is also None.
    assert!(
        queries::update_segment_text(&pool, a.id, uuid::Uuid::new_v4(), "x")
            .await
            .unwrap()
            .is_none()
    );
    // The stored text is still the one legitimate edit.
    let rows = queries::list_segments(&pool, a.id).await.unwrap();
    assert_eq!(rows[0].text, "corrected");
}

#[tokio::test]
async fn folder_is_descendant_terminates_on_a_corrupt_cycle() {
    let pool = memory_pool().await;
    let x = queries::create_folder(&pool, "X", None).await.unwrap();
    let y = queries::create_folder(&pool, "Y", None).await.unwrap();
    // Inject a cycle directly (the reparent guard refuses to create one through the query layer):
    // X -> Y -> X. Both rows exist, so the parent_id FK stays satisfied; only the shape is corrupt.
    for (child, parent) in [(x.id, y.id), (y.id, x.id)] {
        sqlx::query("UPDATE folders SET parent_id = ? WHERE id = ?")
            .bind(parent)
            .bind(child)
            .execute(&pool)
            .await
            .unwrap();
    }
    // Walking from X toward an unrelated ancestor must break on the first revisited node and return
    // false rather than spin the (single) pool connection forever — the visited-set guard. Bound it
    // with a timeout so a regression fails fast instead of hanging the suite.
    let unrelated = uuid::Uuid::new_v4();
    let terminated = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        queries::folder_is_descendant(&pool, x.id, unrelated),
    )
    .await
    .expect("the walk terminates on the corrupt cycle (no infinite loop)")
    .unwrap();
    assert!(!terminated);
    // It still answers a real membership question on the corrupt cycle correctly (Y is X's parent).
    assert!(queries::folder_is_descendant(&pool, x.id, y.id)
        .await
        .unwrap());
}

/// A [`Migrator`] over just the first `count` migrations. sqlx's `run` is otherwise all-or-nothing, so
/// this lets a test seed a DB at an older schema version before upgrading it to head. The extra fields
/// are copied from the embedded [`MIGRATOR`] (they are `#[doc(hidden)]` but public for `migrate!`).
fn prefix_migrator(count: usize) -> Migrator {
    Migrator {
        migrations: Cow::Owned(MIGRATOR.migrations[..count].to_vec()),
        ignore_missing: MIGRATOR.ignore_missing,
        locking: MIGRATOR.locking,
        no_tx: MIGRATOR.no_tx,
        table_name: MIGRATOR.table_name.clone(),
        create_schemas: MIGRATOR.create_schemas.clone(),
    }
}

#[tokio::test]
async fn migrations_upgrade_a_populated_older_db_with_data_intact() {
    // A bare pool (memory_pool runs the full migrator; here we apply a partial one first).
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options("sqlite::memory:").unwrap())
        .await
        .unwrap();

    // Seed at an older schema: apply through 0006 (folders) — before FTS search (0007), the segment
    // `edited` column (0008), and user_notes (0009). The high-level inserts touch only columns present
    // by 0006, so they stand in for data written by an older build.
    assert!(
        MIGRATOR.migrations.len() >= 7,
        "this test seeds at 0006 and upgrades; it assumes >= 7 migrations"
    );
    prefix_migrator(6).run(&pool).await.unwrap();

    let started = chrono::Utc::now();
    let meeting = queries::create_meeting(&pool, "Retro", "/tmp/retro", "", started)
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "we shipped the migration test",
        0.0,
        2.0,
        None,
    )
    .await
    .unwrap();

    // Upgrade to head: the forward-only migrations must apply cleanly on top of the existing rows.
    MIGRATOR.run(&pool).await.unwrap();

    // The seeded rows survived every later migration ...
    let after = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .expect("the seeded meeting survives the upgrade");
    assert_eq!(after.title, "Retro");
    let segs = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert_eq!(segs.len(), 1);
    // ... the column 0008 added defaulted correctly on the pre-existing row ...
    assert!(
        !segs[0].edited,
        "the edited column (0008) defaults to false on rows written before it existed"
    );
    // ... and 0007's FTS backfill ('rebuild') indexed a segment that existed BEFORE the index did, so
    // it is searchable after the upgrade — proof the migration processed pre-existing data, not just
    // new writes.
    assert_eq!(queries::count_search(&pool, "migration*").await.unwrap(), 1);
    let hits = queries::search_segments(&pool, "migration*", 10, 0)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].meeting_id, meeting.id);
}
