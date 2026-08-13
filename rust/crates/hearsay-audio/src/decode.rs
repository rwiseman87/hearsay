//! Block-at-a-time FLAC decoding for the archived meeting audio.
//!
//! Decoding is streamed rather than buffered whole so the verifier can compare a multi-hundred-MB
//! recording against its source without holding either one in memory. The one caller that does want
//! the whole thing — the offline refine's Them track — asks for a single channel, which is the same
//! shape the WAV path already returns.

use std::path::Path;

use crate::error::AudioError;
use crate::SAMPLE_RATE;

/// The channel count declared by a FLAC's stream info.
pub fn flac_channels(path: &Path) -> Result<usize, AudioError> {
    let reader = claxon::FlacReader::open(path)
        .map_err(|e| AudioError::Flac(format!("open {path:?}: {e}")))?;
    Ok(reader.streaminfo().channels as usize)
}

/// Call `visit` with each decoded block in order. The block buffer is recycled between calls, so
/// memory stays O(block) rather than O(meeting).
pub fn for_each_flac_block<F>(path: &Path, mut visit: F) -> Result<(), AudioError>
where
    F: FnMut(&claxon::Block) -> Result<(), AudioError>,
{
    let mut reader = claxon::FlacReader::open(path)
        .map_err(|e| AudioError::Flac(format!("open {path:?}: {e}")))?;
    let rate = reader.streaminfo().sample_rate;
    if rate != SAMPLE_RATE {
        return Err(AudioError::Flac(format!(
            "expected {SAMPLE_RATE} Hz, got {rate}"
        )));
    }
    let mut blocks = reader.blocks();
    let mut buffer = Vec::new();
    loop {
        let block = blocks
            .read_next_or_eof(buffer)
            .map_err(|e| AudioError::Flac(format!("read block: {e}")))?;
        let Some(block) = block else { break };
        visit(&block)?;
        buffer = block.into_buffer();
    }
    Ok(())
}

/// Read a 16 kHz FLAC as mono `f32` in [-1, 1], averaging channels — the FLAC counterpart of the
/// WAV downmix the offline paths use.
pub fn read_flac_mono_16k(path: &Path) -> Result<Vec<f32>, AudioError> {
    let channels = flac_channels(path)?;
    let mut out: Vec<f32> = Vec::new();
    for_each_flac_block(path, |block| {
        for i in 0..block.duration() as usize {
            let sum: i32 = (0..channels).map(|ch| block.channel(ch as u32)[i]).sum();
            out.push(sum as f32 / channels as f32 / 32768.0);
        }
        Ok(())
    })?;
    Ok(out)
}

/// Read one channel of a 16 kHz FLAC as mono `f32` in [-1, 1]. A mono file's only channel is
/// returned whatever `channel` asks for, mirroring the WAV reader's fallback.
///
/// The scaling matches `hearsay_inference::audio`'s WAV path exactly (`/ 32768.0`), so an archived
/// meeting refines to bit-identical input.
pub fn read_flac_channel_16k(path: &Path, channel: usize) -> Result<Vec<f32>, AudioError> {
    let channels = {
        let reader = claxon::FlacReader::open(path)
            .map_err(|e| AudioError::Flac(format!("open {path:?}: {e}")))?;
        reader.streaminfo().channels as usize
    };
    let wanted = if channels <= 1 { 0 } else { channel };
    if wanted >= channels {
        return Err(AudioError::Flac(format!(
            "channel {channel} out of range for a {channels}-channel file"
        )));
    }
    let mut out: Vec<f32> = Vec::new();
    for_each_flac_block(path, |block| {
        out.extend(
            block
                .channel(wanted as u32)
                .iter()
                .map(|v| *v as f32 / 32768.0),
        );
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{interleaved_pattern, stereo_spec, write_wav};
    use crate::{encode_wav_to_flac, CHANNELS};

    fn encoded(dir: &Path, frames: usize) -> (Vec<i16>, std::path::PathBuf) {
        let wav = dir.join("audio.wav");
        let flac = dir.join("audio.flac");
        let samples = interleaved_pattern(frames);
        write_wav(&wav, stereo_spec(), &samples);
        encode_wav_to_flac(&wav, &flac).expect("encode");
        (samples, flac)
    }

    #[test]
    fn reads_the_requested_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let (samples, flac) = encoded(tmp.path(), 5_000);

        for channel in 0..CHANNELS {
            let got = read_flac_channel_16k(&flac, channel).expect("read channel");
            let want: Vec<f32> = samples
                .chunks(CHANNELS)
                .map(|f| f[channel] as f32 / 32768.0)
                .collect();
            assert_eq!(got, want, "channel {channel}");
        }
    }

    #[test]
    fn visits_every_block_once() {
        let tmp = tempfile::tempdir().unwrap();
        let frames = 4096 * 3 + 11;
        let (_, flac) = encoded(tmp.path(), frames);

        let mut blocks = 0usize;
        let mut total = 0usize;
        for_each_flac_block(&flac, |block| {
            blocks += 1;
            total += block.duration() as usize;
            Ok(())
        })
        .expect("walk blocks");
        assert_eq!(blocks, 4);
        assert_eq!(total, frames);
    }

    #[test]
    fn a_mono_file_returns_its_only_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let flac = tmp.path().join("mono.flac");
        write_mono_flac(&flac, &mono_ramp());

        // The Them channel is index 1, but a mono file has only channel 0 — mirroring the WAV
        // reader's fallback so a mono recording still refines.
        let got = read_flac_channel_16k(&flac, 1).expect("read mono");
        assert_eq!(got.len(), mono_ramp().len());
        assert!((got[0] - (-3200.0 / 32768.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn rejects_a_wrong_sample_rate() {
        let tmp = tempfile::tempdir().unwrap();
        let flac = tmp.path().join("fast.flac");
        write_flac_at_rate(&flac, 44_100);
        assert!(matches!(
            read_flac_channel_16k(&flac, 0),
            Err(AudioError::Flac(_))
        ));
    }

    /// `flacenc` requires at least 32 samples per block, so fixtures are sized above that.
    fn mono_ramp() -> Vec<i32> {
        (0..64).map(|i| (i * 100) - 3200).collect()
    }

    /// A minimal single-channel FLAC, written through the same `flacenc` primitives the encoder
    /// uses (which only emits the stereo recorder format).
    fn write_mono_flac(path: &Path, samples: &[i32]) {
        write_flac(path, SAMPLE_RATE as usize, 1, samples);
    }

    fn write_flac_at_rate(path: &Path, rate: usize) {
        write_flac(path, rate, 1, &mono_ramp());
    }

    fn write_flac(path: &Path, rate: usize, channels: usize, interleaved: &[i32]) {
        use flacenc::component::BitRepr;
        use flacenc::error::Verify;

        let config = flacenc::config::Encoder::default().into_verified().unwrap();
        let source = flacenc::source::MemSource::from_samples(interleaved, channels, 16, rate);
        let mut stream =
            flacenc::encode_with_fixed_block_size(&config, source, interleaved.len() / channels)
                .unwrap();
        stream
            .stream_info_mut()
            .set_total_samples(interleaved.len() / channels);
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream.write(&mut sink).unwrap();
        std::fs::write(path, sink.as_slice()).unwrap();
    }
}
