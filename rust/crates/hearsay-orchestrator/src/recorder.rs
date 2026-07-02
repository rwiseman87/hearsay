//! One timeline-accurate stereo `audio.wav` per meeting. Port of
//! `src/hearsay/transcript/recorder.py`.
//!
//! Fed the pipeline's PCM chunks (float in [-1, 1], stamped with a meeting-relative `t0_s` on the
//! shared `host_ts` clock), it places Me on the left channel and Them on the right by meeting time,
//! so sample N is meeting time N/rate. This one file serves playback (`GET /meetings/{id}/audio`)
//! and, later, the offline refine (which reads the Them channel). Held in memory during the meeting
//! and written once at [`close`](MeetingAudioRecorder::close).

use std::path::PathBuf;

use crate::error::OrchestratorError;
use crate::types::Stream;

/// Contract-fixed capture sample rate (Hz), mono per channel.
const SAMPLE_RATE: u32 = 16_000;
/// Normalize so the loudest sample across both channels reaches this fraction of full scale
/// (preserving the Me/Them balance; normalizing by the overall peak keeps a mono downmix in range).
const PLAYBACK_PEAK: f32 = 0.9;
/// Don't amplify an essentially-silent take (would just raise noise).
const MIN_PEAK_TO_NORMALIZE: f32 = 1e-3;
/// Write each stream contiguously; only re-anchor to `t0_s` past this divergence (a real delivery
/// gap, not per-chunk clock jitter). 0.2 s.
const RESYNC_GAP: usize = (SAMPLE_RATE / 5) as usize;

/// Accumulates one timeline-accurate stereo (Me=L, Them=R) 16 kHz WAV.
pub(crate) struct MeetingAudioRecorder {
    path: PathBuf,
    me: Vec<f32>,
    them: Vec<f32>,
    cursor_me: Option<usize>,
    cursor_them: Option<usize>,
}

impl MeetingAudioRecorder {
    /// A recorder that will write `path` (`<folder>/audio.wav`) at close.
    pub(crate) fn new(path: PathBuf) -> Self {
        MeetingAudioRecorder {
            path,
            me: Vec::new(),
            them: Vec::new(),
            cursor_me: None,
            cursor_them: None,
        }
    }

    fn channel_mut(&mut self, stream: Stream) -> (&mut Vec<f32>, &mut Option<usize>) {
        match stream {
            Stream::Me => (&mut self.me, &mut self.cursor_me),
            Stream::Them => (&mut self.them, &mut self.cursor_them),
        }
    }

    /// Place `samples` for `stream` into its channel by meeting time. Written contiguously from the
    /// running cursor; a chunk only jumps to `round(t0_s * rate)` when that diverges past
    /// `RESYNC_GAP` (a real gap), so per-chunk clock jitter never punches holes.
    pub(crate) fn write(&mut self, samples: &[f32], t0_s: f64, stream: Stream) {
        if samples.is_empty() {
            return;
        }
        let target = (t0_s * SAMPLE_RATE as f64).round().max(0.0) as usize;
        let (data, cursor) = self.channel_mut(stream);
        let start = match *cursor {
            Some(c) if target.abs_diff(c) <= RESYNC_GAP => c,
            _ => target,
        };
        let end = start + samples.len();
        if data.len() < end {
            data.resize(end, 0.0);
        }
        for (i, &s) in samples.iter().enumerate() {
            data[start + i] += s;
        }
        *cursor = Some(end);
    }

    /// Normalize to a healthy playback level and write the stereo WAV. A silent / empty take writes
    /// nothing. Best-effort at the call site (a write failure never fails the meeting stop).
    pub(crate) fn close(self) -> Result<(), OrchestratorError> {
        let len = self.me.len().max(self.them.len());
        if len == 0 {
            return Ok(());
        }
        let mut me = self.me;
        let mut them = self.them;
        me.resize(len, 0.0);
        them.resize(len, 0.0);

        let peak = me
            .iter()
            .chain(them.iter())
            .fold(0.0f32, |m, &s| m.max(s.abs()));
        let scale = if peak > MIN_PEAK_TO_NORMALIZE {
            PLAYBACK_PEAK / peak
        } else {
            1.0
        };

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&self.path, spec)
            .map_err(|e| OrchestratorError::Backend(format!("create wav: {e}")))?;
        for i in 0..len {
            for &sample in &[me[i], them[i]] {
                let scaled = (sample * scale).clamp(-1.0, 1.0);
                let pcm = (scaled * 32767.0).round() as i16;
                writer
                    .write_sample(pcm)
                    .map_err(|e| OrchestratorError::Backend(format!("write wav sample: {e}")))?;
            }
        }
        writer
            .finalize()
            .map_err(|e| OrchestratorError::Backend(format!("finalize wav: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_stereo_timeline_and_normalizes_by_overall_peak() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("audio.wav");
        let mut rec = MeetingAudioRecorder::new(path.clone());
        rec.write(&[0.45, 0.45], 0.0, Stream::Me); // Me from meeting time 0
        rec.write(&[-0.45], 0.5, Stream::Them); // Them at 0.5 s -> sample 8000
        rec.close().unwrap();

        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.spec().sample_rate, 16_000);
        let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();

        // len = max(2, 8001) = 8001 frames * 2 channels.
        assert_eq!(samples.len(), 8001 * 2);
        // Overall peak 0.45 -> scaled to 0.9 -> round(0.9 * 32767) = 29490.
        assert!(
            (samples[0] as i32 - 29490).abs() <= 2,
            "Me[0] was {}",
            samples[0]
        );
        assert_eq!(samples[1], 0, "Them is silent at frame 0");
        assert!(
            (samples[8000 * 2 + 1] as i32 + 29490).abs() <= 2,
            "Them[8000] was {}",
            samples[8000 * 2 + 1]
        );
        assert_eq!(samples[8000 * 2], 0, "Me is silent at frame 8000");
    }

    #[test]
    fn silent_take_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("audio.wav");
        MeetingAudioRecorder::new(path.clone()).close().unwrap();
        assert!(!path.exists());
    }
}
