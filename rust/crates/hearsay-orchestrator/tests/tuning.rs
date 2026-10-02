//! The `LiveTuning` seam: the default drops a Me final that echoes Them, and `echo_dedup: None`
//! keeps it.

use std::sync::Arc;
use std::time::Duration;

use hearsay_db::models::Stream as DbStream;
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;
use hearsay_engine::LiveEngine;
use hearsay_orchestrator::testing::{chunk, seg, ProgressiveBackend, ProgressivePlan};
use hearsay_orchestrator::{LiveStats, LiveTuning, Orchestrator, SegmentKind, Stream};

const ECHO: &str = "we should ship the new release on friday afternoon";

async fn me_finals(tuning: LiveTuning) -> usize {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let plan = ProgressivePlan {
        chunks: vec![chunk(Stream::Them, 0, &[0.1; 1600])],
        them: vec![(
            Duration::from_millis(0),
            seg(SegmentKind::Final, ECHO, 0.0, 3.0, Some(0)),
        )],
        me: vec![(
            Duration::from_millis(300),
            seg(SegmentKind::Final, ECHO, 0.2, 3.1, None),
        )],
    };
    let orch = Orchestrator::new(
        pool.clone(),
        tmp.path().to_path_buf(),
        Arc::new(ProgressiveBackend::new(plan)),
    )
    .with_tuning(tuning);
    let meeting = orch.start_meeting(Some("tuning".into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    orch.stop_meeting(meeting.id).await.unwrap();
    orch.wait_for_refines().await;
    queries::list_segments(&pool, meeting.id)
        .await
        .unwrap()
        .iter()
        .filter(|s| s.stream == DbStream::Me)
        .count()
}

#[tokio::test]
async fn default_tuning_drops_an_echoed_me_final() {
    let stats = Arc::new(LiveStats::default());
    let tuning = LiveTuning {
        stats: Some(stats.clone()),
        ..LiveTuning::default()
    };
    assert_eq!(me_finals(tuning).await, 0);
    assert_eq!(stats.echo_drops().len(), 1);
}

#[tokio::test]
async fn disabling_the_dedup_keeps_the_echoed_me_final() {
    let stats = Arc::new(LiveStats::default());
    let tuning = LiveTuning {
        echo_dedup: None,
        stats: Some(stats.clone()),
        ..LiveTuning::default()
    };
    assert_eq!(me_finals(tuning).await, 1);
    assert!(stats.echo_drops().is_empty());
}
