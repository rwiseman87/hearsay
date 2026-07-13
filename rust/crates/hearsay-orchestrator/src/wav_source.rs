//! A file-backed [`AudioSource`] for offline / dev runs: stream a recorded WAV through the
//! pipeline without capture hardware. Reads the canonical stereo `audio.wav` (Me = left, Them =
//! right, 16 kHz — the format `MeetingAudioRecorder` writes), splitting it into timed chunks on the
//! shared `host_ts` clock. A mono file is treated as the Them stream.
//!
//! Like a live source, it holds the channel open until [`stop`](AudioSource::stop) (the caller
//! finalizes the meeting when it has fed enough), so the whole real pipeline can be exercised from a
//! recording — only the audio device is replaced.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::error::OrchestratorError;
use crate::traits::AudioSource;
use crate::types::{AudioChunk, CaptureChunk, Stream};

/// Contract-fixed capture sample rate (Hz), mono per channel.
const SAMPLE_RATE: u32 = 16_000;
/// Default chunk size: 1600 samples = 100 ms at 16 kHz.
const DEFAULT_FRAME_SAMPLES: usize = 1600;

/// Streams a recorded WAV as [`CaptureChunk`]s.
pub struct WavFileSource {
    path: PathBuf,
    frame_samples: usize,
    stop_tx: Option<oneshot::Sender<()>>,
}

impl WavFileSource {
    /// A source that reads `path` (stereo 16 kHz `audio.wav`, or mono treated as Them).
    pub fn new(path: PathBuf) -> Self {
        WavFileSource {
            path,
            frame_samples: DEFAULT_FRAME_SAMPLES,
            stop_tx: None,
        }
    }
}

/// Read a 16 kHz WAV into per-stream f32 sample vectors (Me = left, Them = right; mono -> Them).
fn read_wav(path: &Path) -> Result<(Vec<f32>, Vec<f32>), OrchestratorError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| OrchestratorError::Backend(format!("open wav: {e}")))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE {
        return Err(OrchestratorError::Backend(format!(
            "wav sample rate {} != {SAMPLE_RATE}",
            spec.sample_rate
        )));
    }
    let channels = spec.channels as usize;
    if channels == 0 || channels > 2 {
        return Err(OrchestratorError::Backend(format!(
            "unsupported channel count {channels}"
        )));
    }
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| OrchestratorError::Backend(format!("read wav samples: {e}")))?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| OrchestratorError::Backend(format!("read wav samples: {e}")))?,
        (_, bits) => {
            return Err(OrchestratorError::Backend(format!(
                "unsupported wav sample format ({bits}-bit)"
            )))
        }
    };
    if channels == 1 {
        return Ok((Vec::new(), samples));
    }
    let mut me = Vec::with_capacity(samples.len() / 2);
    let mut them = Vec::with_capacity(samples.len() / 2);
    for pair in samples.chunks_exact(2) {
        me.push(pair[0]);
        them.push(pair[1]);
    }
    Ok((me, them))
}

#[async_trait]
impl AudioSource for WavFileSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        let path = self.path.clone();
        let (me, them) = tokio::task::spawn_blocking(move || read_wav(&path))
            .await
            .map_err(|e| OrchestratorError::Backend(format!("wav read task: {e}")))??;

        let (tx, rx) = mpsc::channel(1024);
        let (stop_tx, stop_rx) = oneshot::channel();
        self.stop_tx = Some(stop_tx);
        let frame = self.frame_samples;

        tokio::spawn(async move {
            let n = me.len().max(them.len());
            let mut start = 0;
            while start < n {
                let host_ts = start as u64 * 1_000_000_000 / SAMPLE_RATE as u64;
                let end = (start + frame).min(n);
                for (stream, data) in [(Stream::Me, &me), (Stream::Them, &them)] {
                    if start < data.len() {
                        let samples = data[start..end.min(data.len())].to_vec();
                        let chunk = CaptureChunk {
                            stream,
                            chunk: AudioChunk { host_ts, samples },
                        };
                        if tx.send(chunk).await.is_err() {
                            return; // pipeline dropped the receiver
                        }
                    }
                }
                start += frame;
            }
            let _ = stop_rx.await; // hold open until stop(); then dropping `tx` closes the channel
        });
        Ok(rx)
    }

    async fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }
}
