//! Sidecar respawn through the full pipeline: a real `ProcessTranscriber` for Me whose sidecar
//! crashes mid-meeting, a scripted Them stream, and a paced capture source so audio keeps flowing
//! across the crash.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use hearsay_db::models::{MeetingStatus, Stream as DbStream};
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;
use hearsay_engine::LiveEngine;
use hearsay_orchestrator::testing::{chunk, seg, ScriptedTranscriber};
use hearsay_orchestrator::{
    AudioSource, Backend, BackendInstance, CaptureChunk, LiveStats, LiveTuning, Orchestrator,
    OrchestratorError, ProcessTranscriber, SegmentKind, Stream,
};

const CHUNK_SAMPLES: usize = 1600;
const CHUNK_NS: u64 = 100_000_000;
const FAST_BACKOFF: Duration = Duration::from_millis(20);

/// Replays chunks one every few milliseconds so the sidecar crash lands mid-stream, then stays open
/// until stopped.
struct PacedSource {
    chunks: Vec<CaptureChunk>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

#[async_trait]
impl AudioSource for PacedSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        let (tx, rx) = mpsc::channel(64);
        let chunks = std::mem::take(&mut self.chunks);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        self.stop = Some(stop_tx);
        tokio::spawn(async move {
            for c in chunks {
                if tx.send(c).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let _ = stop_rx.await;
        });
        Ok(rx)
    }

    async fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Me runs a `ProcessTranscriber` over `me_binary`; Them is scripted so its feed can be inspected.
struct RespawnBackend {
    me_binary: PathBuf,
    me_args: Vec<String>,
    backoff: Vec<Duration>,
    chunks: Mutex<Option<Vec<CaptureChunk>>>,
    them_fed: Arc<Mutex<Vec<f32>>>,
}

impl RespawnBackend {
    fn new(
        me_binary: &str,
        me_args: Vec<String>,
        backoff: Vec<Duration>,
        chunks: Vec<CaptureChunk>,
    ) -> (Arc<Self>, Arc<Mutex<Vec<f32>>>) {
        let them_fed = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(RespawnBackend {
            me_binary: PathBuf::from(me_binary),
            me_args,
            backoff,
            chunks: Mutex::new(Some(chunks)),
            them_fed: them_fed.clone(),
        });
        (backend, them_fed)
    }
}

impl Backend for RespawnBackend {
    fn build(&self) -> BackendInstance {
        let chunks = self.chunks.lock().unwrap().take().unwrap();
        BackendInstance {
            source: Box::new(PacedSource { chunks, stop: None }),
            me: Box::new(
                ProcessTranscriber::new(self.me_binary.clone())
                    .with_args(self.me_args.clone())
                    .with_respawn_backoff(self.backoff.clone())
                    .with_close_timeout(Duration::from_millis(300)),
            ),
            them: Box::new(ScriptedTranscriber::new(
                vec![seg(SegmentKind::Final, "them tail", 0.0, 1.0, Some(0))],
                self.them_fed.clone(),
            )),
        }
    }
}

/// Me chunk `i` carries the value `i + 1` in every sample (so a segment's text names its chunk) at
/// `host_ts = i * 100 ms`; a Them chunk of constant 0.5 rides alongside each.
fn script(count: usize) -> Vec<CaptureChunk> {
    let mut out = Vec::new();
    for i in 0..count {
        let ts = i as u64 * CHUNK_NS;
        out.push(chunk(Stream::Me, ts, &[(i + 1) as f32; CHUNK_SAMPLES]));
        out.push(chunk(Stream::Them, ts, &[0.5; CHUNK_SAMPLES]));
    }
    out
}

fn state_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crashed").to_string_lossy().into_owned();
    (dir, path)
}

async fn me_segments(
    pool: &sqlx::SqlitePool,
    meeting: uuid::Uuid,
) -> Vec<hearsay_db::models::Segment> {
    queries::list_segments(pool, meeting)
        .await
        .unwrap()
        .into_iter()
        .filter(|s| s.stream == DbStream::Me && s.text.starts_with('c'))
        .collect()
}

fn chunk_index(text: &str) -> usize {
    text[1..].parse::<usize>().unwrap()
}

#[tokio::test]
async fn crash_mid_stream_is_recovered_with_correct_meeting_times() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let (_state_dir, state) = state_file();
    let total = 40;
    let (backend, them_fed) = RespawnBackend::new(
        env!("CARGO_BIN_EXE_flaky_sidecar"),
        vec![state, "3".into()],
        vec![FAST_BACKOFF; 3],
        script(total),
    );
    let stats = Arc::new(LiveStats::default());
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend).with_tuning(
        LiveTuning {
            stats: Some(stats.clone()),
            ..LiveTuning::default()
        },
    );
    let meeting = orch.start_meeting(Some("Respawn".into())).await.unwrap();

    // Wait for the replacement sidecar to transcribe late chunks.
    let mut recovered = false;
    for _ in 0..400 {
        if me_segments(&pool, meeting.id)
            .await
            .iter()
            .any(|s| chunk_index(&s.text) >= 20)
        {
            recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(recovered, "no segment from the respawned sidecar arrived");

    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    orch.wait_for_refines().await;

    let segs = me_segments(&pool, meeting.id).await;
    assert!(segs.iter().any(|s| chunk_index(&s.text) <= 3));
    // Every segment, before and after the crash, lands at its chunk's meeting time even though the
    // replacement sidecar's own clock restarted at 0.
    for s in &segs {
        let expected = (chunk_index(&s.text) - 1) as f64 * 0.1;
        assert!(
            (s.start_s - expected).abs() < 1e-6,
            "segment {} at {} expected {}",
            s.text,
            s.start_s,
            expected
        );
    }
    // The crash left a gap rather than a replay: nothing was fed twice.
    let mut indices: Vec<usize> = segs.iter().map(|s| chunk_index(&s.text)).collect();
    let before = indices.len();
    indices.dedup();
    assert_eq!(before, indices.len());

    // Them was never disturbed.
    assert_eq!(them_fed.lock().unwrap().len(), total * CHUNK_SAMPLES);
    assert_eq!(stats.me_respawns.load(Ordering::SeqCst), 1);
    assert_eq!(stats.them_respawns.load(Ordering::SeqCst), 0);
}

/// Them runs a `ProcessTranscriber` over the flaky sidecar; Me is scripted and silent.
struct ThemRespawnBackend {
    state: String,
    chunks: Mutex<Option<Vec<CaptureChunk>>>,
}

impl Backend for ThemRespawnBackend {
    fn build(&self) -> BackendInstance {
        let chunks = self.chunks.lock().unwrap().take().unwrap();
        BackendInstance {
            source: Box::new(PacedSource { chunks, stop: None }),
            me: Box::new(ScriptedTranscriber::new(vec![], Arc::default())),
            them: Box::new(
                ProcessTranscriber::new(PathBuf::from(env!("CARGO_BIN_EXE_flaky_sidecar")))
                    .with_args(vec![self.state.clone(), "3".into()])
                    .with_respawn_backoff(vec![FAST_BACKOFF; 3])
                    .with_close_timeout(Duration::from_millis(300)),
            ),
        }
    }
}

/// Them chunk `i` carries `i + 1` in every sample; a silent Me chunk rides alongside each.
fn them_script(count: usize) -> Vec<CaptureChunk> {
    let mut out = Vec::new();
    for i in 0..count {
        let ts = i as u64 * CHUNK_NS;
        out.push(chunk(Stream::Me, ts, &[0.0; CHUNK_SAMPLES]));
        out.push(chunk(Stream::Them, ts, &[(i + 1) as f32; CHUNK_SAMPLES]));
    }
    out
}

#[tokio::test]
async fn them_respawn_numbers_speakers_under_new_clusters() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let (_state_dir, state) = state_file();
    let backend = Arc::new(ThemRespawnBackend {
        state,
        chunks: Mutex::new(Some(them_script(40))),
    });
    let stats = Arc::new(LiveStats::default());
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend).with_tuning(
        LiveTuning {
            stats: Some(stats.clone()),
            ..LiveTuning::default()
        },
    );
    let meeting = orch
        .start_meeting(Some("Them respawn".into()))
        .await
        .unwrap();

    let them_segments = || async {
        queries::list_segments(&pool, meeting.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|s| s.stream == DbStream::Them && s.text.starts_with('c'))
            .collect::<Vec<_>>()
    };
    for _ in 0..400 {
        if them_segments()
            .await
            .iter()
            .any(|s| chunk_index(&s.text) >= 20)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    orch.wait_for_refines().await;
    assert_eq!(stats.them_respawns.load(Ordering::SeqCst), 1);

    let segs = them_segments().await;
    let (before, after): (Vec<_>, Vec<_>) = segs.iter().partition(|s| chunk_index(&s.text) <= 3);
    assert!(!before.is_empty());
    assert!(!after.is_empty(), "no segment from the respawned sidecar");
    let old = before[0].cluster_id.unwrap();
    let new = after[0].cluster_id.unwrap();
    assert_ne!(
        old, new,
        "the replacement's speaker 0 must not join the old cluster"
    );
    assert!(before
        .iter()
        .all(|s| s.cluster_id == Some(old) && s.speaker_label == "Speaker 1"));
    assert!(after
        .iter()
        .all(|s| s.cluster_id == Some(new) && s.speaker_label == "Speaker 2"));
    let ordinals: Vec<i64> = queries::list_speaker_rows(&pool, meeting.id)
        .await
        .unwrap()
        .iter()
        .map(|r| r.ordinal)
        .collect();
    assert_eq!(ordinals, vec![1, 2]);
}

#[tokio::test]
async fn retry_exhaustion_degrades_without_ending_the_meeting() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    // "-" crashes every incarnation after one frame, so the budget runs out.
    let total = 40;
    let (backend, them_fed) = RespawnBackend::new(
        env!("CARGO_BIN_EXE_flaky_sidecar"),
        vec!["-".into(), "1".into()],
        vec![FAST_BACKOFF; 3],
        script(total),
    );
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend);
    let meeting = orch.start_meeting(Some("Exhaust".into())).await.unwrap();

    for _ in 0..400 {
        if them_fed.lock().unwrap().len() == total * CHUNK_SAMPLES {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        them_fed.lock().unwrap().len(),
        total * CHUNK_SAMPLES,
        "Them must keep being fed after Me gives up"
    );
    assert_eq!(orch.active_meeting(), Some(meeting.id));

    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    orch.wait_for_refines().await;

    // One frame from the original sidecar plus at most one per restart.
    let segs = me_segments(&pool, meeting.id).await;
    assert!(!segs.is_empty() && segs.len() <= 4, "got {}", segs.len());
    let them = queries::list_segments(&pool, meeting.id).await.unwrap();
    assert!(them
        .iter()
        .any(|s| s.stream == DbStream::Them && s.text == "them tail"));
}

#[tokio::test]
async fn sidecar_that_dies_on_every_spawn_exhausts_and_the_meeting_finalizes() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let total = 30;
    let (backend, them_fed) = RespawnBackend::new(
        env!("CARGO_BIN_EXE_dying_sidecar"),
        vec![],
        vec![FAST_BACKOFF; 3],
        script(total),
    );
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend);
    let meeting = orch.start_meeting(Some("Dying".into())).await.unwrap();

    for _ in 0..400 {
        if them_fed.lock().unwrap().len() == total * CHUNK_SAMPLES {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(them_fed.lock().unwrap().len(), total * CHUNK_SAMPLES);

    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    orch.wait_for_refines().await;
    assert!(me_segments(&pool, meeting.id).await.is_empty());
}

#[tokio::test]
async fn silent_hung_sidecar_does_not_disturb_them_and_stop_is_bounded() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let total = 20;
    let (backend, them_fed) = RespawnBackend::new(
        env!("CARGO_BIN_EXE_hang_sidecar"),
        vec![],
        vec![FAST_BACKOFF; 3],
        script(total),
    );
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend);
    let meeting = orch.start_meeting(Some("Hang".into())).await.unwrap();

    for _ in 0..400 {
        if them_fed.lock().unwrap().len() == total * CHUNK_SAMPLES {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(them_fed.lock().unwrap().len(), total * CHUNK_SAMPLES);

    let stopped = tokio::time::timeout(Duration::from_secs(20), orch.stop_meeting(meeting.id))
        .await
        .expect("stop must be bounded")
        .unwrap()
        .unwrap();
    assert_eq!(stopped.status, MeetingStatus::Finalized);
    orch.wait_for_refines().await;
}
