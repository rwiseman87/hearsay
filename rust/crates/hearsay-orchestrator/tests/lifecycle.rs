//! Meeting lifecycle over the `LiveEngine` seam, driven end-to-end with scripted fakes against an
//! in-memory SQLite DB: no real audio or model sidecars. Covers start (row + folder + active),
//! PCM routing to the right transcriber, the partial/final broadcast contract, final persistence
//! (Me + Them `Speaker N` clusters), the meeting-time offset shift, stop (finalize + clear), the
//! busy guard, and stopping an unknown meeting.

use std::sync::Arc;

use hearsay_core::{LiveEngine, LiveError};
use hearsay_db::models::MeetingStatus;
use hearsay_db::{connect_options, queries, MIGRATOR};
use hearsay_orchestrator::testing::ScriptedBackend;
use hearsay_orchestrator::{
    AudioChunk, Backend, CaptureChunk, Orchestrator, SegmentKind, SidecarSegment, Stream,
};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use tokio::sync::broadcast::error::RecvError;

async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options("sqlite::memory:").unwrap())
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

fn chunk(stream: Stream, host_ts: u64, samples: &[f32]) -> CaptureChunk {
    CaptureChunk {
        stream,
        chunk: AudioChunk {
            host_ts,
            samples: samples.to_vec(),
        },
    }
}

fn seg(
    kind: SegmentKind,
    text: &str,
    start_s: f64,
    end_s: f64,
    speaker: Option<i64>,
) -> SidecarSegment {
    SidecarSegment {
        kind,
        text: text.to_string(),
        start_s,
        end_s,
        speaker,
    }
}

fn orchestrator(
    pool: SqlitePool,
    dir: &std::path::Path,
    backend: Arc<dyn Backend>,
) -> Orchestrator {
    Orchestrator::new(pool, dir.to_path_buf(), backend)
}

/// Drain every buffered broadcast line (the fakes emit their segments at stop, and the 256-slot
/// channel buffers them, so a post-stop drain sees them all) until the senders close.
async fn drain(mut rx: tokio::sync::broadcast::Receiver<String>) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        match rx.recv().await {
            Ok(line) => out.push(serde_json::from_str(&line).unwrap()),
            Err(RecvError::Closed) => break,
            Err(RecvError::Lagged(_)) => continue,
        }
    }
    out
}

#[tokio::test]
async fn full_lifecycle_routes_persists_and_broadcasts() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    // Me anchors the epoch (t0=0); Them arrives 0.5s later (offset 0.5s), so its segment times
    // shift by +0.5 into meeting time.
    let chunks = vec![
        chunk(Stream::Me, 1_000_000_000, &[0.1, 0.2]),
        chunk(Stream::Them, 1_500_000_000, &[0.3, 0.4, 0.5]),
    ];
    let me_segments = vec![
        seg(SegmentKind::Partial, "hello", 0.0, 0.5, None),
        seg(SegmentKind::Final, "hello there", 0.0, 1.0, None),
    ];
    let them_segments = vec![
        seg(SegmentKind::Partial, "hi", 0.0, 0.5, None),
        seg(SegmentKind::Final, "hi everyone", 1.0, 2.0, Some(0)),
    ];
    let (backend, fed) = ScriptedBackend::new(chunks, me_segments, them_segments);
    let orch = orchestrator(pool.clone(), tmp.path(), backend);

    let meeting = orch
        .start_meeting(Some("Weekly Sync".into()))
        .await
        .unwrap();
    assert_eq!(meeting.status, MeetingStatus::Recording);
    assert!(
        meeting.folder.ends_with("_weekly-sync"),
        "folder was {}",
        meeting.folder
    );
    assert!(tmp.path().join(&meeting.folder).is_dir());
    assert_eq!(orch.active_meeting(), Some(meeting.id));

    // Subscribe before stop so every emitted event is buffered for us.
    let rx = orch
        .subscribe(meeting.id)
        .expect("active meeting is subscribable");

    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    assert!(stopped.ended_at.is_some());
    assert_eq!(orch.active_meeting(), None);
    assert!(orch.subscribe(meeting.id).is_none());

    // PCM routed to the right transcriber.
    assert_eq!(*fed.me.lock().unwrap(), vec![0.1, 0.2]);
    assert_eq!(*fed.them.lock().unwrap(), vec![0.3, 0.4, 0.5]);

    // Broadcast contract: partials + finals for both streams (Them partial speaker-less, Them final
    // labeled + offset-shifted by 0.5s).
    let events = drain(rx).await;
    assert_eq!(events.len(), 4);
    let find = |kind: &str, stream: &str| {
        events
            .iter()
            .find(|e| e["kind"] == kind && e["stream"] == stream)
            .unwrap_or_else(|| panic!("missing {kind}/{stream} event"))
    };
    assert_eq!(find("partial", "me")["speaker_label"], "Me");
    assert_eq!(find("final", "me")["text"], "hello there");
    assert_eq!(find("partial", "them")["speaker_label"], "Them");
    let them_final = find("final", "them");
    assert_eq!(them_final["speaker_label"], "Speaker 1");
    assert_eq!(them_final["start_s"], 1.5);
    assert_eq!(them_final["end_s"], 2.5);

    // Persistence: only finals; Them bound to a `Speaker 1` cluster.
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert_eq!(segments.len(), 2);
    let me = segments.iter().find(|s| s.stream == Stream::Me).unwrap();
    assert_eq!(me.speaker_label, "Me");
    assert_eq!(me.text, "hello there");
    assert_eq!(me.cluster_id, None);
    let them = segments.iter().find(|s| s.stream == Stream::Them).unwrap();
    assert_eq!(them.speaker_label, "Speaker 1");
    assert_eq!(them.start_s, 1.5);

    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    assert_eq!(speakers.len(), 1);
    assert_eq!(speakers[0].ordinal, 1);
    assert_eq!(them.cluster_id, Some(speakers[0].id));
}

#[tokio::test]
async fn start_while_recording_is_busy() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let (backend, _fed) = ScriptedBackend::new(vec![], vec![], vec![]);
    let orch = orchestrator(pool, tmp.path(), backend);

    let first = orch.start_meeting(None).await.unwrap();
    assert_eq!(orch.active_meeting(), Some(first.id));

    let err = orch.start_meeting(None).await.unwrap_err();
    assert!(matches!(err, LiveError::Busy(_)));
}

#[tokio::test]
async fn stop_unknown_meeting_is_none() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let (backend, _fed) = ScriptedBackend::new(vec![], vec![], vec![]);
    let orch = orchestrator(pool, tmp.path(), backend);

    let result = orch.stop_meeting(uuid::Uuid::new_v4()).await.unwrap();
    assert!(result.is_none());
}
