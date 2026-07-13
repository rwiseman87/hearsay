//! One timeline-accurate stereo `audio.wav` per meeting, written incrementally. Port of
//! `src/hearsay/transcript/recorder.py`.
//!
//! Fed the pipeline's PCM chunks (float in [-1, 1], stamped with a meeting-relative `t0_s` on the
//! shared `host_ts` clock), it places Me on the left channel and Them on the right by meeting time,
//! so sample N is meeting time N/rate. This one file serves playback (`GET /meetings/{id}/audio`)
//! and the offline refine (which reads the Them channel).
//!
//! **Streaming, not buffered-whole-meeting.** A stereo frame is encoded to the WAV as soon as both
//! channels have data for it (interleave up to `min(me_end, them_end)`), so only the small
//! inter-stream skew is held in memory — RAM is O(skew), not O(meeting length). There is no global
//! peak normalization (that would need every sample first, i.e. the whole meeting in RAM); samples
//! are written at captured level, clamped to [-1, 1] so a stray over can't overflow the 16-bit
//! encoding. Playback gain is a UI concern (the web player boosts quiet recordings).

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use crate::error::OrchestratorError;
use crate::types::Stream;

/// Contract-fixed capture sample rate (Hz), mono per channel.
const SAMPLE_RATE: u32 = 16_000;
/// Write each stream contiguously; only re-anchor to `t0_s` past this divergence (a real delivery
/// gap, not per-chunk clock jitter). 0.2 s.
const RESYNC_GAP: usize = (SAMPLE_RATE / 5) as usize;
/// Cap the in-memory skew between the two channels. If one stream ever runs this far ahead of the
/// other (a stalled stream), flush the leader with the laggard zero-filled rather than buffer
/// without bound. 30 s — far beyond real inter-stream jitter, so it never fires in normal capture;
/// it is only a hard ceiling on memory.
const MAX_SKEW_FRAMES: usize = 30 * SAMPLE_RATE as usize;

/// Accumulates one timeline-accurate stereo (Me=L, Them=R) 16 kHz WAV, encoding frames as they
/// complete so memory stays bounded by the inter-stream skew.
pub(crate) struct MeetingAudioRecorder {
    path: PathBuf,
    /// Created on the first encoded frame — a meeting with no audio writes no file.
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    /// Absolute stereo frames already encoded to the WAV.
    written: usize,
    /// Un-encoded tail per channel; `me[i]` / `them[i]` is meeting frame `written + i`.
    me: Vec<f32>,
    them: Vec<f32>,
    /// Absolute frame cursor per channel (end of the last write), for gap resync.
    cursor_me: Option<usize>,
    cursor_them: Option<usize>,
    /// A create/write failure latches the recorder off — best-effort, never fails the meeting.
    failed: bool,
}

fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

/// Captured level, clamped so an over (or an overlapping-placement sum) can't wrap the 16-bit range.
fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

impl MeetingAudioRecorder {
    /// A recorder that will write `path` (`<folder>/audio.wav`) as the meeting streams.
    pub(crate) fn new(path: PathBuf) -> Self {
        MeetingAudioRecorder {
            path,
            writer: None,
            written: 0,
            me: Vec::new(),
            them: Vec::new(),
            cursor_me: None,
            cursor_them: None,
            failed: false,
        }
    }

    fn channel_mut(&mut self, stream: Stream) -> (&mut Vec<f32>, &mut Option<usize>) {
        match stream {
            Stream::Me => (&mut self.me, &mut self.cursor_me),
            Stream::Them => (&mut self.them, &mut self.cursor_them),
        }
    }

    /// Place `samples` for `stream` into its channel by meeting time, then encode any newly-complete
    /// frames. Written contiguously from the running cursor; a chunk only jumps to `round(t0_s *
    /// rate)` when that diverges past `RESYNC_GAP` (a real gap), so per-chunk clock jitter never
    /// punches holes. Positions already encoded to disk cannot be rewritten (a backward jump past
    /// the flushed frontier — which a monotonic clock never produces — is clamped forward).
    pub(crate) fn write(&mut self, samples: &[f32], t0_s: f64, stream: Stream) {
        if self.failed || samples.is_empty() {
            return;
        }
        let written = self.written;
        let target = (t0_s * SAMPLE_RATE as f64).round().max(0.0) as usize;
        {
            let (data, cursor) = self.channel_mut(stream);
            let start_abs = match *cursor {
                Some(c) if target.abs_diff(c) <= RESYNC_GAP => c,
                _ => target,
            }
            .max(written);
            let start = start_abs - written;
            let end = start + samples.len();
            if data.len() < end {
                data.resize(end, 0.0);
            }
            for (i, &s) in samples.iter().enumerate() {
                data[start + i] += s;
            }
            *cursor = Some(start_abs + samples.len());
        }
        self.flush(false);
    }

    /// Encode every frame both channels now cover (up to `min(me, them)`). When `closing`, drain
    /// everything, zero-filling the shorter channel to the longer. Otherwise, if the skew exceeds
    /// `MAX_SKEW_FRAMES` (a stalled stream), force the leader out with the laggard zero-filled so
    /// memory stays bounded.
    fn flush(&mut self, closing: bool) {
        if self.failed {
            return;
        }
        let longer = self.me.len().max(self.them.len());
        let ready = if closing {
            longer
        } else {
            let paired = self.me.len().min(self.them.len());
            if longer - paired > MAX_SKEW_FRAMES {
                longer - MAX_SKEW_FRAMES
            } else {
                paired
            }
        };
        if ready == 0 {
            return;
        }
        if self.writer.is_none() {
            match hound::WavWriter::create(&self.path, wav_spec()) {
                Ok(writer) => self.writer = Some(writer),
                Err(e) => {
                    tracing::error!(error = %e, path = %self.path.display(), "failed to create meeting audio.wav");
                    self.failed = true;
                    return;
                }
            }
        }
        let writer = self.writer.as_mut().expect("writer created above");
        for i in 0..ready {
            let left = to_i16(self.me.get(i).copied().unwrap_or(0.0));
            let right = to_i16(self.them.get(i).copied().unwrap_or(0.0));
            if writer.write_sample(left).is_err() || writer.write_sample(right).is_err() {
                tracing::error!(path = %self.path.display(), "failed writing meeting audio.wav");
                self.failed = true;
                return;
            }
        }
        self.me.drain(0..ready.min(self.me.len()));
        self.them.drain(0..ready.min(self.them.len()));
        self.written += ready;
    }

    /// Flush the tail (zero-filling the shorter channel to the longer) and finalize the WAV. A
    /// meeting with no audio writes no file. Best-effort at the call site (a write failure never
    /// fails the meeting stop).
    pub(crate) fn close(mut self) -> Result<(), OrchestratorError> {
        self.flush(true);
        if self.failed {
            return Ok(()); // already logged; never fail the meeting stop
        }
        match self.writer.take() {
            Some(writer) => writer
                .finalize()
                .map_err(|e| OrchestratorError::Backend(format!("finalize wav: {e}"))),
            None => Ok(()), // silent take: nothing was ever written
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_stereo_timeline_at_captured_level() {
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

        // len = max(2, 8001) = 8001 frames * 2 channels; the shorter channel is zero-filled.
        assert_eq!(samples.len(), 8001 * 2);
        // Captured level, not normalized: round(0.45 * 32767) = 14745.
        assert!(
            (samples[0] as i32 - 14745).abs() <= 1,
            "Me[0] was {}",
            samples[0]
        );
        assert_eq!(samples[1], 0, "Them is silent at frame 0");
        assert!(
            (samples[8000 * 2 + 1] as i32 + 14745).abs() <= 1,
            "Them[8000] was {}",
            samples[8000 * 2 + 1]
        );
        assert_eq!(samples[8000 * 2], 0, "Me is silent at frame 8000");
    }

    #[test]
    fn interleaves_contiguous_streams_frame_for_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("audio.wav");
        let mut rec = MeetingAudioRecorder::new(path.clone());
        let me_chunk = [0.5_f32; 1600];
        let them_chunk = [-0.25_f32; 1600];
        // Both streams arrive contiguously, interleaved by arrival (the normal capture cadence).
        for i in 0..4 {
            let t = i as f64 * 0.1; // 0.1 s == 1600 samples/chunk
            rec.write(&me_chunk, t, Stream::Me);
            rec.write(&them_chunk, t, Stream::Them);
        }
        rec.close().unwrap();

        let reader = hound::WavReader::open(&path).unwrap();
        let samples: Vec<i16> = reader.into_samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples.len(), 4 * 1600 * 2);
        // Every frame carries Me on the left, Them on the right, at captured level.
        assert!(
            (samples[0] as i32 - 16384).abs() <= 1,
            "Me[0] {}",
            samples[0]
        );
        assert!(
            (samples[1] as i32 + 8192).abs() <= 1,
            "Them[0] {}",
            samples[1]
        );
        let last = samples.len() - 2;
        assert!(
            (samples[last] as i32 - 16384).abs() <= 1,
            "Me[last] {}",
            samples[last]
        );
    }

    #[test]
    fn silent_take_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("audio.wav");
        MeetingAudioRecorder::new(path.clone()).close().unwrap();
        assert!(!path.exists());
    }
}
