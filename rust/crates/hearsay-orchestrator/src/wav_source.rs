//! A file-backed [`AudioSource`] for offline / dev runs: stream a recorded meeting through the
//! pipeline without capture hardware. Reads the canonical stereo `audio.wav` (Me = left, Them =
//! right, 16 kHz — the format `MeetingAudioRecorder` writes), splitting it into timed chunks on the
//! shared `host_ts` clock. A mono file is treated as the Them stream.
//!
//! Like a live source, it holds the channel open until [`stop`](AudioSource::stop) (the caller
//! finalizes the meeting when it has fed enough), so the whole real pipeline can be exercised from a
//! recording — only the audio device is replaced.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::error::OrchestratorError;
use crate::traits::AudioSource;
use crate::types::{AudioChunk, CaptureChunk, Stream};

use hearsay_audio::SAMPLE_RATE;
/// Default chunk size: 1600 samples = 100 ms at 16 kHz.
const DEFAULT_FRAME_SAMPLES: usize = 1600;

/// Streams a recorded WAV as [`CaptureChunk`]s.
pub struct WavFileSource {
    path: PathBuf,
    frame_samples: usize,
    speed: f64,
    stop_tx: Option<oneshot::Sender<()>>,
}

impl WavFileSource {
    /// A source that reads `path` (stereo 16 kHz `audio.wav`, or mono treated as Them).
    pub fn new(path: PathBuf) -> Self {
        WavFileSource {
            path,
            frame_samples: DEFAULT_FRAME_SAMPLES,
            speed: 0.0,
            stop_tx: None,
        }
    }

    /// Pace the replay at `speed` times real time (1.0 is real time). The default, 0.0 (or any
    /// non-positive value), replays as fast as the pipeline accepts.
    pub fn with_speed(mut self, speed: f64) -> Self {
        self.speed = speed;
        self
    }
}

/// How long after the replay starts the chunk at sample `start` is due, at `speed` times real time.
fn due_after(start: usize, speed: f64) -> Duration {
    Duration::from_secs_f64(start as f64 / SAMPLE_RATE as f64 / speed)
}

/// Read a 16 kHz recording into per-stream f32 sample vectors (Me = left, Them = right; mono ->
/// Them). Accepts the archived FLAC as well as the WAV, so replaying a meeting keeps working after
/// the storage sweep has compressed it.
fn read_recording(path: &Path) -> Result<(Vec<f32>, Vec<f32>), OrchestratorError> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("flac"))
    {
        return read_flac(path);
    }
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
    Ok(split_stereo(&samples))
}

/// Read the archived FLAC form of the same recording.
fn read_flac(path: &Path) -> Result<(Vec<f32>, Vec<f32>), OrchestratorError> {
    let channels = hearsay_audio::flac_channels(path)
        .map_err(|e| OrchestratorError::Backend(format!("read flac: {e}")))?;
    let read = |ch| {
        hearsay_audio::read_flac_channel_16k(path, ch)
            .map_err(|e| OrchestratorError::Backend(format!("read flac: {e}")))
    };
    // Mono is Them only, matching the wav path.
    if channels <= 1 {
        return Ok((Vec::new(), read(0)?));
    }
    Ok((read(0)?, read(1)?))
}

/// Split interleaved stereo into (Me = left, Them = right).
fn split_stereo(samples: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut me = Vec::with_capacity(samples.len() / 2);
    let mut them = Vec::with_capacity(samples.len() / 2);
    for pair in samples.chunks_exact(2) {
        me.push(pair[0]);
        them.push(pair[1]);
    }
    (me, them)
}

#[async_trait]
impl AudioSource for WavFileSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        let path = self.path.clone();
        let (me, them) = tokio::task::spawn_blocking(move || read_recording(&path))
            .await
            .map_err(|e| OrchestratorError::Backend(format!("wav read task: {e}")))??;

        let (tx, rx) = mpsc::channel(1024);
        let (stop_tx, stop_rx) = oneshot::channel();
        self.stop_tx = Some(stop_tx);
        let frame = self.frame_samples;
        let speed = self.speed;

        tokio::spawn(async move {
            let n = me.len().max(them.len());
            let began = tokio::time::Instant::now();
            let mut start = 0;
            while start < n {
                if speed > 0.0 {
                    tokio::time::sleep_until(began + due_after(start, speed)).await;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_stereo(path: &Path, frames: usize) -> Vec<i16> {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut samples = Vec::with_capacity(frames * 2);
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..frames as i32 {
            let me = ((i * 5) % 7_001 - 3_500) as i16;
            let them = ((i * 11) % 15_001 - 7_500) as i16;
            writer.write_sample(me).unwrap();
            writer.write_sample(them).unwrap();
            samples.push(me);
            samples.push(them);
        }
        writer.finalize().unwrap();
        samples
    }

    /// Replaying a meeting must survive the storage sweep compressing it — the dev output dir is
    /// the same tree the sweep walks.
    #[test]
    fn reads_an_archived_flac_identically_to_its_wav() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("audio.wav");
        let flac = tmp.path().join("audio.flac");
        write_stereo(&wav, 5_000);
        hearsay_audio::encode_wav_to_flac(&wav, &flac).expect("encode");

        let (me_wav, them_wav) = read_recording(&wav).expect("read wav");
        let (me_flac, them_flac) = read_recording(&flac).expect("read flac");

        assert_eq!(me_wav.len(), 5_000);
        assert_eq!(me_wav, me_flac);
        assert_eq!(them_wav, them_flac);
    }

    #[test]
    fn pacing_scales_the_due_time_by_speed() {
        assert_eq!(due_after(16_000, 1.0), Duration::from_secs(1));
        assert_eq!(due_after(16_000, 4.0), Duration::from_millis(250));
    }

    #[tokio::test]
    async fn a_paced_source_spreads_chunks_over_the_scaled_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("audio.wav");
        write_stereo(&wav, 16_000 * 2);
        let mut source = WavFileSource::new(wav).with_speed(8.0);
        let mut rx = source.start().await.unwrap();
        let began = std::time::Instant::now();
        let mut last = Duration::ZERO;
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            last = began.elapsed();
        }
        assert!(
            last >= Duration::from_millis(200),
            "2 s of audio at 8x ends near 0.24 s, got {last:?}"
        );
        source.stop().await;
    }

    #[test]
    fn a_mono_wav_is_treated_as_them_only() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("mono.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..2_000i32 {
            writer.write_sample((i % 1_000) as i16).unwrap();
        }
        writer.finalize().unwrap();

        let (me, them) = read_recording(&wav).expect("read wav");
        assert!(me.is_empty(), "mono has no Me channel");
        assert_eq!(them.len(), 2_000);
    }
}
