//! The dev-only scripted engine (`HEARSAY_SCRIPTED`): a canned meeting whose transcript emits
//! progressively over the live broadcast *during* recording, then persists on stop. This is the
//! model-free engine the browser E2E drives; the test proves the progressive-emit property (which the
//! stop-time `ScriptedBackend` in the full-stack test cannot show) plus the on-disk persistence.

use std::path::PathBuf;
use std::time::Duration;

use sqlx::SqlitePool;

use hearsay_backends::{build_scripted_engine, EngineConfig, LoopbackMode};
use hearsay_db::models::{MeetingStatus, Stream};
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;

/// A minimal [`EngineConfig`]: the scripted engine reads only `pool` / `output_dir` / the editable
/// defaults, so the platform paths are placeholders. `record` is off so no `audio.wav` is written.
fn config(pool: SqlitePool, output_dir: PathBuf) -> EngineConfig {
    EngineConfig {
        pool,
        output_dir,
        helper_path: PathBuf::from("unused"),
        synthetic: false,
        prewarm: true,
        refine_model: PathBuf::new(),
        refine_timeout: Duration::from_secs(60),
        record: false,
        auto_refine: true,
        recognition_threshold: 0.6,
        inactivity_prompt: false,
        inactivity_auto_end: false,
        inactivity_prompt_minutes: 5,
        inactivity_end_minutes: 10,
        notes_enabled: false,
        notes_model: PathBuf::new(),
        notes_prompt: String::new(),
        notes_binary: PathBuf::from("unused"),
        sherpa_models_dir: PathBuf::new(),
        win_loopback_mode: LoopbackMode::Device,
    }
}

#[tokio::test]
async fn scripted_engine_emits_progressively_and_persists() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let engine = build_scripted_engine(config(pool.clone(), tmp.path().to_path_buf()));

    let meeting = engine.start_meeting(Some("Scripted".into())).await.unwrap();
    let mut rx = engine
        .subscribe(meeting.id)
        .expect("subscribe to the active meeting");

    // Progressive emit: both finals arrive over the live broadcast WHILE recording, before any stop —
    // the property the browser E2E relies on (and the stop-time `ScriptedBackend` cannot show). Watch
    // until both stream finals have streamed in, mirroring the E2E's "watch the transcript populate,
    // then stop"; a stop before Them's final (~1.2 s) would drop it, exactly as in a real meeting.
    let (mut me_final, mut them_final) = (false, false);
    while !(me_final && them_final) {
        let Ok(Ok(line)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await else {
            break; // timed out or the broadcast closed
        };
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        if frame["kind"] == "final" {
            match frame["stream"].as_str() {
                Some("me") => me_final = true,
                Some("them") => them_final = true,
                _ => {}
            }
        }
    }
    assert!(
        me_final && them_final,
        "both finals broadcast live during recording (me={me_final}, them={them_final})"
    );

    // Stop finalizes the meeting and its two finals persist (partials never do).
    let stopped = engine
        .stop_meeting(meeting.id)
        .await
        .unwrap()
        .expect("stopped meeting");
    assert_eq!(stopped.status, MeetingStatus::Finalized);

    let segs = queries::list_segments(&pool, meeting.id).await.unwrap();
    let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
    assert!(
        texts.contains(&"hello there"),
        "Me final persisted: {texts:?}"
    );
    assert!(
        texts.contains(&"hi everyone, thanks for joining"),
        "Them final persisted: {texts:?}"
    );
    // Them was diarized into a Speaker 1 cluster (the channel + diarizer-ordinal binding).
    let them = segs
        .iter()
        .find(|s| s.stream == Stream::Them)
        .expect("a Them segment");
    assert_eq!(them.speaker_label, "Speaker 1");

    engine.shutdown().await;
}
