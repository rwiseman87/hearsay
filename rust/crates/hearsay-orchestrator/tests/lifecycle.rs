//! Meeting lifecycle over the `LiveEngine` seam, driven end-to-end with scripted fakes against an
//! in-memory SQLite DB: no real audio or model sidecars. Covers start (row + folder + active),
//! PCM routing to the right transcriber, the partial/final broadcast contract, final persistence
//! (Me + Them `Speaker N` clusters), the meeting-time offset shift, stop (finalize + clear), the
//! busy guard, and stopping an unknown meeting.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use hearsay_db::models::MeetingStatus;
use hearsay_db::{connect_options, queries, MIGRATOR};
use hearsay_engine::{LiveEngine, LiveError};
use hearsay_orchestrator::testing::{ScriptedBackend, ScriptedRefiner};
use hearsay_orchestrator::{
    AudioChunk, Backend, CaptureChunk, Orchestrator, RefinedThemSegment, SegmentKind,
    SidecarSegment, Stream,
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

/// A wired refiner replaces the live Them guesses at stop (auto-refine), and the persisted segments
/// + clusters reflect the refine's output.
#[tokio::test]
async fn stop_auto_refines_them_segments() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    // Live path yields a single Them "Speaker 1" final; the refiner replaces it with two speakers.
    let chunks = vec![chunk(Stream::Them, 1_000_000_000, &[0.1, 0.2, 0.3])];
    let them_segments = vec![seg(SegmentKind::Final, "live guess", 0.0, 1.0, Some(0))];
    let (backend, _fed) = ScriptedBackend::new(chunks, vec![], them_segments);

    let refined = vec![
        RefinedThemSegment {
            ordinal: 1,
            text: "refined one".into(),
            start_s: 0.0,
            end_s: 1.0,
        },
        RefinedThemSegment {
            ordinal: 2,
            text: "refined two".into(),
            start_s: 1.0,
            end_s: 2.0,
        },
    ];
    let (refiner, calls) = ScriptedRefiner::new(refined);
    let orch = orchestrator(pool.clone(), tmp.path(), backend).with_refiner(refiner);

    let meeting = orch.start_meeting(Some("Refine Me".into())).await.unwrap();
    // Auto-refine runs only when an `audio.wav` exists; the refiner ignores its content.
    std::fs::write(
        tmp.path().join(&meeting.folder).join("audio.wav"),
        b"placeholder",
    )
    .unwrap();

    orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The single live Them final is replaced by the refiner's two speakers.
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let them: Vec<_> = segments
        .iter()
        .filter(|s| s.stream == Stream::Them)
        .collect();
    assert_eq!(them.len(), 2);
    assert_eq!(them[0].text, "refined one");
    assert_eq!(them[0].speaker_label, "Speaker 1");
    assert_eq!(them[1].speaker_label, "Speaker 2");

    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    assert_eq!(speakers.len(), 2);
}

/// A refine error is best-effort: the stop still finalizes and the live Them segments are kept.
#[tokio::test]
async fn stop_auto_refine_error_keeps_live_segments() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    let chunks = vec![chunk(Stream::Them, 1_000_000_000, &[0.1, 0.2, 0.3])];
    let them_segments = vec![seg(SegmentKind::Final, "live guess", 0.0, 1.0, Some(0))];
    let (backend, _fed) = ScriptedBackend::new(chunks, vec![], them_segments);

    let (refiner, calls) = ScriptedRefiner::failing("diarize sidecar exploded");
    let orch = orchestrator(pool.clone(), tmp.path(), backend).with_refiner(refiner);

    let meeting = orch.start_meeting(None).await.unwrap();
    std::fs::write(
        tmp.path().join(&meeting.folder).join("audio.wav"),
        b"placeholder",
    )
    .unwrap();

    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The live guess survives (refine failed, so nothing was replaced).
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let them: Vec<_> = segments
        .iter()
        .filter(|s| s.stream == Stream::Them)
        .collect();
    assert_eq!(them.len(), 1);
    assert_eq!(them[0].text, "live guess");
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

#[tokio::test]
async fn record_setting_off_skips_audio_wav() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let chunks = vec![chunk(Stream::Them, 1_000_000_000, &[0.1, 0.2])];
    let (backend, _fed) = ScriptedBackend::new(chunks, vec![], vec![]);

    // The config default is record=on; the stored UI override turns per-meeting audio off, and the
    // orchestrator reads that override at meeting start.
    queries::set_preference(&pool, queries::SECTION_RECORDING, r#"{"record":false}"#)
        .await
        .unwrap();

    let orch = orchestrator(pool, tmp.path(), backend);
    let meeting = orch.start_meeting(Some("No Rec".into())).await.unwrap();
    orch.stop_meeting(meeting.id).await.unwrap().unwrap();

    let dir = std::path::PathBuf::from(&meeting.dir);
    assert!(dir.is_dir(), "meeting dir should still be created");
    assert!(
        !dir.join("audio.wav").exists(),
        "record=false must skip the audio.wav recorder"
    );
}

#[tokio::test]
async fn storage_override_pins_meeting_dir_off_the_default_root() {
    let pool = memory_pool().await;
    let default_root = tempfile::tempdir().unwrap();
    let override_root = tempfile::tempdir().unwrap();
    let (backend, _fed) = ScriptedBackend::new(vec![], vec![], vec![]);

    queries::set_preference(
        &pool,
        queries::SECTION_STORAGE,
        &format!(
            r#"{{"output_dir":"{}"}}"#,
            override_root.path().to_str().unwrap()
        ),
    )
    .await
    .unwrap();

    let orch = orchestrator(pool.clone(), default_root.path(), backend);
    let meeting = orch.start_meeting(Some("Elsewhere".into())).await.unwrap();
    orch.stop_meeting(meeting.id).await.unwrap().unwrap();

    // The meeting was created under the override root (not the default), and its dir was pinned +
    // persisted so later playback/refine/delete locate it regardless of the current setting.
    assert!(override_root.path().join(&meeting.folder).is_dir());
    assert!(!default_root.path().join(&meeting.folder).exists());
    let refetched = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refetched.dir, meeting.dir);
    assert_eq!(
        refetched.dir_path(default_root.path()),
        override_root.path().join(&meeting.folder)
    );
}
