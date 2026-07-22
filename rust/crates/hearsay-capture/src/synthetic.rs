//! Generated-audio capture source: the `SYNTHETIC=1` plumbing on platforms without the Swift
//! helper (whose `--synthetic` flag serves that role on macOS). Emits alternating tone bursts on
//! Me and Them at real-time pace, stamped from one monotonic clock, so the whole pipeline — clock
//! anchoring, the recorder, transcribers, watchdog — runs without touching a device or raising a
//! permission prompt. Platform-neutral, so its cadence and shape are unit-tested on any host.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use hearsay_orchestrator::{AudioChunk, AudioSource, CaptureChunk, OrchestratorError, Stream};

/// Contract-fixed capture sample rate (Hz), mono per stream.
const SAMPLE_RATE: usize = 16_000;
/// One emitted chunk per stream per tick.
const CHUNK: Duration = Duration::from_millis(100);
/// Samples per chunk (100 ms at 16 kHz).
const CHUNK_SAMPLES: usize = SAMPLE_RATE / 10;
/// The streams take turns "speaking" in windows of this length (Me on even windows, Them on odd),
/// so both streams carry speech-shaped activity without overlapping.
const TURN: Duration = Duration::from_secs(2);

/// A synthetic capture source: Me = 440 Hz bursts, Them = 660 Hz bursts, alternating.
#[derive(Default)]
pub struct SyntheticSource {
    task: Option<JoinHandle<()>>,
}

impl SyntheticSource {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AudioSource for SyntheticSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        let (tx, rx) = mpsc::channel::<CaptureChunk>(1024);
        self.task = Some(tokio::spawn(generate(tx)));
        Ok(rx)
    }

    async fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

/// Emit one chunk per stream per tick until the receiver drops (or the task is aborted by stop).
async fn generate(tx: mpsc::Sender<CaptureChunk>) {
    let epoch = Instant::now();
    let mut interval = tokio::time::interval(CHUNK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut emitted: u64 = 0;
    loop {
        interval.tick().await;
        let host_ts = epoch.elapsed().as_nanos() as u64;
        // Which stream is "speaking" this window; the other emits silence.
        let window = (emitted as u128 * CHUNK.as_nanos() / TURN.as_nanos()) as u64;
        for (stream, tone_hz) in [(Stream::Me, 440.0f32), (Stream::Them, 660.0f32)] {
            let speaking = match stream {
                Stream::Me => window.is_multiple_of(2),
                Stream::Them => !window.is_multiple_of(2),
            };
            let samples = if speaking {
                tone(tone_hz, emitted)
            } else {
                vec![0.0; CHUNK_SAMPLES]
            };
            let chunk = CaptureChunk {
                stream,
                chunk: AudioChunk { host_ts, samples },
            };
            if tx.send(chunk).await.is_err() {
                return; // the orchestrator dropped the receiver
            }
        }
        emitted += 1;
    }
}

/// One chunk of a continuous sine at `hz`, phase-continuous across chunks via the chunk index.
fn tone(hz: f32, chunk_index: u64) -> Vec<f32> {
    let start = chunk_index as usize * CHUNK_SAMPLES;
    (0..CHUNK_SAMPLES)
        .map(|i| {
            let t = (start + i) as f32 / SAMPLE_RATE as f32;
            0.3 * (TAU * hz * t).sin()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emits_both_streams_with_monotonic_timestamps() {
        let mut source = SyntheticSource::new();
        let mut rx = source.start().await.unwrap();

        let mut me = 0usize;
        let mut them = 0usize;
        let mut last_ts: Option<u64> = None;
        // 3 ticks x 2 streams.
        for _ in 0..6 {
            let cap = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("chunk within deadline")
                .expect("channel open");
            assert_eq!(cap.chunk.samples.len(), CHUNK_SAMPLES);
            if let Some(prev) = last_ts {
                assert!(cap.chunk.host_ts >= prev, "host_ts must be monotonic");
            }
            last_ts = Some(cap.chunk.host_ts);
            match cap.stream {
                Stream::Me => me += 1,
                Stream::Them => them += 1,
            }
        }
        assert_eq!((me, them), (3, 3));

        source.stop().await;
        // After stop the generator task is gone; the channel drains then closes.
        while rx.recv().await.is_some() {}
    }

    #[test]
    fn tone_is_phase_continuous_across_chunks() {
        let a = tone(440.0, 0);
        let b = tone(440.0, 1);
        // The first sample of chunk 1 continues the sine where chunk 0 left off.
        let expected = 0.3 * (TAU * 440.0 * (CHUNK_SAMPLES as f32 / SAMPLE_RATE as f32)).sin();
        assert!((b[0] - expected).abs() < 1e-4);
        assert_eq!(a.len(), CHUNK_SAMPLES);
    }
}
