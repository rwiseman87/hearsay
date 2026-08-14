//! `ProcessTranscriber` end-to-end over the `mock_sidecar` fixture binary: spawn a real process,
//! feed framed PCM on its stdin, read the NDJSON segments it emits, and close (draining the
//! finalized tail). Exercises the real stdio path that will back `hearsay-inference`.

use std::path::PathBuf;
use std::time::Duration;

use hearsay_orchestrator::{ProcessTranscriber, SegmentKind, Transcriber};

#[tokio::test]
async fn process_transcriber_spawns_feeds_and_drains() {
    let mut transcriber =
        ProcessTranscriber::new(PathBuf::from(env!("CARGO_BIN_EXE_mock_sidecar")));
    let mut rx = transcriber.start().await.unwrap();

    transcriber.feed(vec![0.1, 0.2]).await;
    transcriber.feed(vec![0.3]).await;
    transcriber.close().await; // EOF -> per-frame finals + a tail, then the channel closes

    let mut segments = Vec::new();
    while let Some(seg) = rx.recv().await {
        segments.push(seg);
    }

    // Two per-frame finals (one per fed chunk) + one tail on EOF, in order.
    assert_eq!(segments.len(), 3);
    assert!(segments.iter().all(|s| s.kind == SegmentKind::Final));
    assert_eq!(segments[0].text, "chunk 0");
    assert_eq!(segments[1].text, "chunk 1");
    assert_eq!(segments[2].text, "tail");
}

/// `spawn_warming()` spawns the sidecar (its models loading in the background), then `start()`
/// adopts that same running process (its receiver stashed at spawn time) rather than spawning a
/// second one — so the model load happens off the meeting-start path and only one process ever
/// loads. Feeding still works and the ready marker is not surfaced as a segment.
#[tokio::test]
async fn spawn_warming_then_start_reuses_the_prewarmed_process() {
    let mut transcriber =
        ProcessTranscriber::new(PathBuf::from(env!("CARGO_BIN_EXE_mock_sidecar")));
    transcriber.spawn_warming().unwrap();
    let mut rx = transcriber.start().await.unwrap();

    transcriber.feed(vec![0.1, 0.2]).await;
    transcriber.close().await; // EOF -> one per-frame final + a tail, then the channel closes

    let mut segments = Vec::new();
    while let Some(seg) = rx.recv().await {
        segments.push(seg);
    }

    // The ready marker is filtered out, so only the fed frame's final + the EOF tail remain.
    assert_eq!(segments.len(), 2);
    assert!(segments.iter().all(|s| s.kind == SegmentKind::Final));
    assert_eq!(segments[0].text, "chunk 0");
    assert_eq!(segments[1].text, "tail");
}

/// A prewarmed sidecar that is still running reports `is_alive()`; one that has exited (a failed
/// model load) reports not-alive, so the warm pool can evict the dead pair and re-warm instead of
/// leaving it to wedge the "Start" gate. `mock_sidecar` blocks on stdin (alive); `dying_sidecar`
/// exits immediately (dead).
#[tokio::test]
async fn is_alive_reflects_whether_the_sidecar_process_is_running() {
    let mut live = ProcessTranscriber::new(PathBuf::from(env!("CARGO_BIN_EXE_mock_sidecar")));
    live.spawn_warming().unwrap();
    assert!(live.is_alive(), "a running sidecar must report alive");
    live.close().await;

    let mut dead = ProcessTranscriber::new(PathBuf::from(env!("CARGO_BIN_EXE_dying_sidecar")));
    dead.spawn_warming().unwrap();
    // The fixture exits at once; wait (bounded) for is_alive() to observe the exit.
    let mut observed_dead = false;
    for _ in 0..100 {
        if !dead.is_alive() {
            observed_dead = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        observed_dead,
        "a sidecar that exited must report not-alive so the pool can evict it"
    );
}

/// A wedged sidecar that never closes stdout (and never exits) must not hang `close()`: the stdout
/// drain is bounded and then the child is killed. A short injected deadline keeps the test fast; the
/// outer timeout is a safety net that fails the test instead of hanging the run.
#[tokio::test]
async fn close_is_bounded_when_sidecar_never_closes_stdout() {
    let mut transcriber =
        ProcessTranscriber::new(PathBuf::from(env!("CARGO_BIN_EXE_hang_sidecar")))
            .with_close_timeout(Duration::from_millis(200));
    let _rx = transcriber.start().await.unwrap();

    tokio::time::timeout(Duration::from_secs(10), transcriber.close())
        .await
        .expect("close() must be bounded on a wedged sidecar, not hang");
}
