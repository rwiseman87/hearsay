//! Startup crash-recovery: a meeting left non-terminal by a hard exit is finalized on boot.

use sqlx::types::chrono::Utc;

use hearsay_backends::reconcile::reconcile_stranded_meetings;
use hearsay_db::models::MeetingStatus;
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;

#[tokio::test]
async fn reconcile_finalizes_a_meeting_left_recording() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    // A meeting that was started but never stopped is left `recording` with no `ended_at` -- exactly
    // the stranded state a SIGKILL / power-loss leaves behind. An empty `dir` makes `dir_path` resolve
    // to `output_dir/<folder>`.
    let meeting = queries::create_meeting(&pool, "Stranded sync", "recon-test", "", Utc::now())
        .await
        .unwrap();
    assert_eq!(meeting.status, MeetingStatus::Recording);
    assert!(meeting.ended_at.is_none());

    reconcile_stranded_meetings(&pool, tmp.path()).await;

    let after = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .expect("meeting still present");
    assert_eq!(after.status, MeetingStatus::Finalized);
    assert!(after.ended_at.is_some(), "ended_at is stamped on finalize");

    // The transcript + metadata were (re)written from the persisted segments under the meeting dir.
    let dir = tmp.path().join("recon-test");
    assert!(dir.join("transcript.md").exists(), "transcript.md written");
    assert!(dir.join("meeting.json").exists(), "meeting.json written");
}

#[tokio::test]
async fn reconcile_is_a_noop_without_stranded_meetings() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    // Nothing non-terminal: the sweep does nothing, writes nothing, and never panics.
    reconcile_stranded_meetings(&pool, tmp.path()).await;

    assert!(
        std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
        "no meeting directories created"
    );
}
