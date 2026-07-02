//! Capstone: the entire pure-Rust live pipeline on the Mac, no Swift and no capture hardware — a
//! recorded WAV (`WavFileSource`) drives two `SherpaTranscriber`s (sherpa streaming ASR) through the
//! real `Orchestrator` into SQLite. This is the Windows live path minus real capture: prove the
//! streaming ASR -> orchestrator -> persisted transcript chain works end-to-end. Ignored by default
//! (needs the 20M streaming model + a recording).
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-core streaming_pipeline -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hearsay_core::SherpaTranscriber;
use hearsay_db::models::Stream;
use hearsay_db::{connect_options, queries, MIGRATOR};
use hearsay_engine::LiveEngine;
use hearsay_inference::{StreamingAsr, StreamingModel};
use hearsay_orchestrator::{Backend, BackendInstance, Orchestrator, WavFileSource};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options("sqlite::memory:").unwrap())
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

fn load_streaming(dir: &Path) -> StreamingAsr {
    StreamingAsr::load(StreamingModel {
        encoder: &dir.join("encoder-epoch-99-avg-1.int8.onnx"),
        decoder: &dir.join("decoder-epoch-99-avg-1.int8.onnx"),
        joiner: &dir.join("joiner-epoch-99-avg-1.int8.onnx"),
        tokens: &dir.join("tokens.txt"),
    })
    .expect("load streaming model")
}

/// A backend wiring a recorded WAV (as capture) to two pure-Rust sherpa live transcribers.
struct SherpaFileBackend {
    wav: PathBuf,
    model_dir: PathBuf,
}

impl Backend for SherpaFileBackend {
    fn build(&self) -> BackendInstance {
        BackendInstance {
            source: Box::new(WavFileSource::new(self.wav.clone())),
            me: Box::new(SherpaTranscriber::new(load_streaming(&self.model_dir))),
            them: Box::new(SherpaTranscriber::new(load_streaming(&self.model_dir))),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the 20M streaming zipformer model + a recorded meeting"]
async fn wav_through_sherpa_streaming_persists_transcript() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();

    let backend = Arc::new(SherpaFileBackend {
        wav: repo("outputs/recordings/2026-07-01_1833_miguel-kristina-test2/audio.wav"),
        model_dir: repo("outputs/models/sherpa-onnx-streaming-zipformer-en-20M-2023-02-17"),
    });
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend);

    let meeting = orch
        .start_meeting(Some("streaming e2e".into()))
        .await
        .unwrap();
    // stop drains capture -> both live sessions flush -> persistence + transcript.md before returning.
    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert!(stopped.ended_at.is_some());

    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    eprintln!("persisted {} finalized segments", segments.len());
    for seg in segments.iter().take(6) {
        eprintln!("  [{:?} {}] {}", seg.stream, seg.speaker_label, seg.text);
    }
    assert!(
        !segments.is_empty(),
        "expected finalized segments from the live sessions"
    );
    // The Them track (2 speakers) produced real transcript text; live is speaker-less so it binds
    // "Speaker 1" (the offline refine would assign real speakers).
    let them: Vec<_> = segments
        .iter()
        .filter(|s| s.stream == Stream::Them)
        .collect();
    assert!(!them.is_empty(), "expected Them segments");
    assert!(
        them.iter().any(|s| !s.text.trim().is_empty()),
        "expected non-empty Them transcript text"
    );

    // The folder story: transcript.md + meeting.json.
    let folder = tmp.path().join(&meeting.folder);
    let transcript = std::fs::read_to_string(folder.join("transcript.md")).unwrap();
    assert!(transcript.starts_with("# streaming e2e\n"));
    let meta = std::fs::read_to_string(folder.join("meeting.json")).unwrap();
    assert!(meta.contains("\"status\": \"finalized\""));
}
