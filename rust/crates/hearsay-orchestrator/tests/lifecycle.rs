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
use hearsay_orchestrator::testing::{
    CrashingBackend, EmptyBackend, FailingBackend, GateRefiner, ScriptedBackend, ScriptedRefiner,
    WarmingBackend, WedgeMeBackend,
};
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
    // No refiner wired, so stop finalizes directly (no interim `refining`).
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    assert!(stopped.ended_at.is_some());
    assert_eq!(orch.active_meeting(), None);
    assert!(orch.subscribe(meeting.id).is_none());
    orch.wait_for_refines().await; // let the background transcript write finish

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

    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    // Stop returns immediately with an interim `refining` status; the refine runs in the background.
    assert_eq!(stopped.status, MeetingStatus::Refining);
    orch.wait_for_refines().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Once the background refine completes, the meeting flips to `finalized`.
    let finalized = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finalized.status, MeetingStatus::Finalized);

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
    assert_eq!(stopped.status, MeetingStatus::Refining);
    orch.wait_for_refines().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A failed refine is best-effort: the meeting still finalizes.
    let finalized = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finalized.status, MeetingStatus::Finalized);

    // The live guess survives (refine failed, so nothing was replaced).
    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let them: Vec<_> = segments
        .iter()
        .filter(|s| s.stream == Stream::Them)
        .collect();
    assert_eq!(them.len(), 1);
    assert_eq!(them[0].text, "live guess");
}

/// The refine is off the op-lock: after a stop returns (status `refining`), a new meeting can start
/// while the previous one is still refining in the background.
#[tokio::test]
async fn start_succeeds_while_previous_meeting_refines() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    let (refiner, gate) = GateRefiner::new();
    let orch = orchestrator(pool.clone(), tmp.path(), Arc::new(EmptyBackend)).with_refiner(refiner);

    let first = orch.start_meeting(Some("First".into())).await.unwrap();
    // Auto-refine runs only when an `audio.wav` exists; the gated refiner ignores its content.
    std::fs::write(
        tmp.path().join(&first.folder).join("audio.wav"),
        b"placeholder",
    )
    .unwrap();

    let stopped = orch.stop_meeting(first.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, MeetingStatus::Refining);
    assert_eq!(orch.active_meeting(), None);

    // The background refine has begun and is blocked — so stop returned before it finished.
    gate.started.notified().await;

    // A new meeting starts even though the first is still refining (the op-lock is free).
    let second = orch.start_meeting(Some("Second".into())).await.unwrap();
    assert_eq!(orch.active_meeting(), Some(second.id));

    // Release the refine and let it finish; the first meeting then finalizes.
    gate.release.notify_one();
    orch.wait_for_refines().await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    let finalized = queries::get_meeting(&pool, first.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finalized.status, MeetingStatus::Finalized);
}

/// Double-stop guard: stopping an already-finalized meeting returns its row unchanged without
/// rewriting `ended_at` or launching a second refine.
#[tokio::test]
async fn double_stop_does_not_rerefine() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    let chunks = vec![chunk(Stream::Them, 1_000_000_000, &[0.1, 0.2, 0.3])];
    let them_segments = vec![seg(SegmentKind::Final, "live guess", 0.0, 1.0, Some(0))];
    let (backend, _fed) = ScriptedBackend::new(chunks, vec![], them_segments);
    let (refiner, calls) = ScriptedRefiner::new(vec![RefinedThemSegment {
        ordinal: 1,
        text: "refined".into(),
        start_s: 0.0,
        end_s: 1.0,
    }]);
    let orch = orchestrator(pool.clone(), tmp.path(), backend).with_refiner(refiner);

    let meeting = orch.start_meeting(None).await.unwrap();
    std::fs::write(
        tmp.path().join(&meeting.folder).join("audio.wav"),
        b"placeholder",
    )
    .unwrap();

    let first = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    orch.wait_for_refines().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let finalized = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    let ended_at = finalized.ended_at;

    // A second stop on the finalized meeting is a no-op: same row back, no re-refine.
    let second = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    orch.wait_for_refines().await;
    assert_eq!(second.status, MeetingStatus::Finalized);
    assert_eq!(second.ended_at, ended_at);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the refine must not run twice"
    );
    // first was the interim `refining` row; ended_at was stamped once and never rewritten.
    assert_eq!(first.ended_at, ended_at);
}

/// A delivery gap in the capture timeline (dropped frames / a tap rebuild / a chunk dropped under
/// backpressure) is padded with silence into the sidecar, so its sample-count timeline — and thus
/// transcript times — stay aligned with meeting time (and `audio.wav`, which re-anchors on `t0_s`).
#[tokio::test]
async fn timeline_gap_pads_sidecar_with_silence() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    // Them: 0.1 s of audio at t0=0, then the next chunk at t0=0.5 s — a 0.4 s gap with only 0.1 s
    // of samples fed, so the resync pads 0.4 s (6400 samples) of silence before it.
    let chunks = vec![
        chunk(Stream::Them, 0, &[1.0; 1600]),
        chunk(Stream::Them, 500_000_000, &[2.0; 1600]),
    ];
    let (backend, fed) = ScriptedBackend::new(chunks, vec![], vec![]);
    let orch = orchestrator(pool.clone(), tmp.path(), backend);

    let meeting = orch.start_meeting(Some("Gap".into())).await.unwrap();
    orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    orch.wait_for_refines().await;

    let them = fed.them.lock().unwrap();
    assert_eq!(
        them.len(),
        1600 + 6400 + 1600,
        "the 0.4 s gap is padded to 6400 silence samples"
    );
    assert!(them[0..1600].iter().all(|&s| s == 1.0));
    assert!(them[1600..8000].iter().all(|&s| s == 0.0), "gap is silence");
    assert!(them[8000..9600].iter().all(|&s| s == 2.0));
}

/// A wedged sidecar (its `feed` does not return) must not starve the other stream: demux
/// drops-with-log for the wedged stream instead of blocking, so Them is fed in full even though Me
/// is stuck. Were demux to block on the full Me queue (the pre-fix behavior), it would never reach
/// the Them chunks that follow and Them would stay empty (the test would fail fast, not hang).
#[tokio::test]
async fn wedged_sidecar_does_not_starve_other_stream() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    // 150 Me chunks (enough to overflow the 128-slot hand-off) ahead of the Them chunks. Them's
    // 1600-sample chunks match their 100 ms spacing, so its timeline is contiguous (no resync pad).
    let mut chunks: Vec<CaptureChunk> = (0..150)
        .map(|i| chunk(Stream::Me, i as u64 * 100_000_000, &[1.0; 100]))
        .collect();
    for i in 0..5 {
        chunks.push(chunk(Stream::Them, i as u64 * 100_000_000, &[2.0; 1600]));
    }
    let (backend, them_fed, release) = WedgeMeBackend::new(chunks);
    let orch = orchestrator(pool.clone(), tmp.path(), backend);

    let meeting = orch.start_meeting(Some("Wedge".into())).await.unwrap();

    // While Me is wedged, wait for demux to feed Them in full — proof it reached the Them chunks past
    // the full Me queue without blocking. Bounded spins so the pre-fix behavior fails fast.
    let mut fed_all = false;
    for _ in 0..10_000 {
        if them_fed.lock().unwrap().len() == 5 * 1600 {
            fed_all = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(fed_all, "Them must be fed in full despite Me being wedged");

    // Release Me so its stream task winds down cleanly at stop (no bounded-join abort in the test).
    release.notify_one();
    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    orch.wait_for_refines().await;
}

/// An unexpected capture death (helper crash / socket EOF, not an intentional stop) finalizes the
/// meeting: the supervisor clears the active session and finalizes the row, so it is never left
/// falsely `recording`/active with a dead pipeline.
#[tokio::test]
async fn capture_death_finalizes_the_meeting() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let backend = Arc::new(CrashingBackend::new(vec![chunk(
        Stream::Them,
        0,
        &[0.1, 0.2, 0.3],
    )]));
    let orch = Arc::new(Orchestrator::new(
        pool.clone(),
        tmp.path().to_path_buf(),
        backend,
    ));
    orch.install_self();

    let meeting = orch.start_meeting(Some("Crash".into())).await.unwrap();
    assert_eq!(orch.active_meeting(), Some(meeting.id));

    // The source closes on its own (helper crash); the supervisor finalizes the row + clears active.
    let mut row = None;
    for _ in 0..10_000 {
        let m = queries::get_meeting(&pool, meeting.id)
            .await
            .unwrap()
            .unwrap();
        if m.status != MeetingStatus::Recording {
            row = Some(m);
            break;
        }
        tokio::task::yield_now().await;
    }
    let row = row.expect("capture death must finalize the meeting row (not left recording)");
    assert!(row.ended_at.is_some());
    assert_eq!(
        orch.active_meeting(),
        None,
        "active session must be cleared"
    );
    orch.wait_for_refines().await;
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

/// A start failure (a sidecar that will not spawn) must not strand a `recording` meeting row and
/// must leave nothing active — the row is deleted and the folder cleaned up.
#[tokio::test]
async fn start_failure_leaves_no_recording_row() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let orch = orchestrator(pool.clone(), tmp.path(), Arc::new(FailingBackend));

    let err = orch.start_meeting(Some("Doomed".into())).await.unwrap_err();
    assert!(matches!(err, LiveError::Internal(_)));

    assert_eq!(orch.active_meeting(), None);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meetings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "a failed start must not leave a meeting row");
    // The folder was created before the pipeline failed; it must be cleaned up too.
    let entries = std::fs::read_dir(tmp.path()).unwrap().count();
    assert_eq!(
        entries, 0,
        "a failed start must not leave an empty meeting folder"
    );
}

/// A cold start reports `transcription_warming == Some(true)` until every sidecar signals ready;
/// when the last one does, it flips to `Some(false)` and a `{"kind":"status","state":"ready"}`
/// event is broadcast so a connected client can clear its warm-up notice.
#[tokio::test]
async fn transcription_warming_flips_when_sidecars_report_ready() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let (backend, me_ready, them_ready) = WarmingBackend::new();
    let orch = orchestrator(pool.clone(), tmp.path(), backend);

    let meeting = orch.start_meeting(None).await.unwrap();
    assert_eq!(
        orch.transcription_warming(meeting.id),
        Some(true),
        "cold sidecars should report warming until they load"
    );
    // A meeting id that is not the active session has no warm-up state.
    assert_eq!(orch.transcription_warming(uuid::Uuid::new_v4()), None);

    let mut rx = orch
        .subscribe(meeting.id)
        .expect("active meeting is subscribable");

    // Fire both ready signals; the pipeline watcher flips warming false and broadcasts the status.
    me_ready.send(()).unwrap();
    them_ready.send(()).unwrap();

    let status = loop {
        match rx.recv().await {
            Ok(line) => break serde_json::from_str::<Value>(&line).unwrap(),
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => panic!("channel closed before the ready status arrived"),
        }
    };
    assert_eq!(status["kind"], "status");
    assert_eq!(status["state"], "ready");
    assert_eq!(
        orch.transcription_warming(meeting.id),
        Some(false),
        "warming should clear once both sidecars are ready"
    );

    orch.stop_meeting(meeting.id).await.unwrap();
    orch.wait_for_refines().await;
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
    orch.wait_for_refines().await;

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
    orch.wait_for_refines().await;

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
