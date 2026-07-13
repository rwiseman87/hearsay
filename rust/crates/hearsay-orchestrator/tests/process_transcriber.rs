//! `ProcessTranscriber` end-to-end over the `mock_sidecar` fixture binary: spawn a real process,
//! feed framed PCM on its stdin, read the NDJSON segments it emits, and close (draining the
//! finalized tail). Exercises the real stdio path that will back `hearsay-inference`.

use std::path::PathBuf;

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
