//! Capstone: the full orchestrator pipeline driven from a recorded WAV through real process
//! sidecars — only the audio device (a `WavFileSource`) and the ML model (the `mock_sidecar`
//! fixture) are stand-ins. Proves the real source + real `ProcessTranscriber` + real `Orchestrator`
//! compose and persist segments end-to-end, with no capture hardware.

use std::path::PathBuf;
use std::sync::Arc;

use hearsay_db::models::Stream;
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;
use hearsay_engine::LiveEngine;
use hearsay_orchestrator::{
    Backend, BackendInstance, Orchestrator, ProcessTranscriber, WavFileSource,
};

/// A backend wiring a WAV file (as the capture source) to two `mock_sidecar` process transcribers.
struct FileProcessBackend {
    wav: PathBuf,
    sidecar: PathBuf,
}

impl Backend for FileProcessBackend {
    fn build(&self) -> BackendInstance {
        BackendInstance {
            source: Box::new(WavFileSource::new(self.wav.clone())),
            me: Box::new(ProcessTranscriber::new(self.sidecar.clone())),
            them: Box::new(ProcessTranscriber::new(self.sidecar.clone())),
        }
    }
}

fn write_stereo_wav(path: &std::path::Path, frames_of_samples: usize) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for _ in 0..frames_of_samples {
        writer.write_sample(8000i16).unwrap();
        writer.write_sample(-8000i16).unwrap();
    }
    writer.finalize().unwrap();
}

#[tokio::test]
async fn wav_through_process_sidecars_persists_segments() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join("audio.wav");
    write_stereo_wav(&wav, 3200); // 2 frames per channel

    let backend = Arc::new(FileProcessBackend {
        wav,
        sidecar: PathBuf::from(env!("CARGO_BIN_EXE_mock_sidecar")),
    });
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend);

    let meeting = orch.start_meeting(Some("e2e".into())).await.unwrap();
    // stop drains capture -> both sidecars -> persistence + the audio.wav write before it returns;
    // the transcript.md / meeting.json write runs on the post-stop background task.
    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert!(stopped.ended_at.is_some());
    orch.wait_for_refines().await; // join the background transcript write

    let folder = tmp.path().join(&meeting.folder);

    // The recorder wrote a stereo audio.wav (re-encoding the input through the pipeline).
    let reader = hound::WavReader::open(folder.join("audio.wav")).expect("audio.wav written");
    assert_eq!(reader.spec().channels, 2);
    assert_eq!(reader.spec().sample_rate, 16_000);

    // transcript.md renders the finalized turns; meeting.json describes the folder.
    let transcript = std::fs::read_to_string(folder.join("transcript.md")).unwrap();
    assert!(transcript.starts_with("# e2e\n"));
    assert!(transcript.contains("### "));
    assert!(transcript.contains("Speaker 1"));
    assert!(transcript.contains("chunk 0"));
    let meta = std::fs::read_to_string(folder.join("meeting.json")).unwrap();
    assert!(meta.contains("\"status\": \"finalized\""));

    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert!(
        !segments.is_empty(),
        "expected persisted finals from the sidecars"
    );
    // Me finals are labeled "Me"; Them finals bind a `Speaker 1` cluster (the mock emits no speaker,
    // so ordinal defaults to 1).
    assert!(segments
        .iter()
        .any(|s| s.stream == Stream::Me && s.speaker_label == "Me"));
    assert!(segments
        .iter()
        .any(|s| s.stream == Stream::Them && s.speaker_label == "Speaker 1"));

    let speakers = queries::list_speaker_rows(&pool, meeting.id).await.unwrap();
    assert_eq!(speakers.len(), 1);
    assert_eq!(speakers[0].ordinal, 1);
}
