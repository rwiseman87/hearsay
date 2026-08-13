//! Shared WAV fixtures for this crate's tests.

use std::path::Path;

use crate::{CHANNELS, SAMPLE_RATE};

pub(crate) fn stereo_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: CHANNELS as u16,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

/// Write `samples` (already interleaved) as a WAV at `path`.
pub(crate) fn write_wav(path: &Path, spec: hound::WavSpec, samples: &[i16]) {
    let mut writer = hound::WavWriter::create(path, spec).expect("create wav");
    for s in samples {
        writer.write_sample(*s).expect("write sample");
    }
    writer.finalize().expect("finalize wav");
}

/// `frames` stereo frames of content chosen to exercise the encoder rather than compress trivially:
/// a tone on the left, a pseudo-random signal on the right, punctuated by digital silence and
/// full-scale extremes (the values most likely to expose a clamping or sign bug).
pub(crate) fn interleaved_pattern(frames: usize) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * CHANNELS);
    let mut noise: u32 = 0x1234_5678;
    for i in 0..frames {
        noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let (left, right) = match i % 512 {
            0..=63 => (0, 0),
            64 => (i16::MAX, i16::MIN),
            65 => (i16::MIN, i16::MAX),
            _ => {
                let tone = ((i as f32 * 0.05).sin() * 12_000.0) as i16;
                (tone, (noise >> 16) as i16)
            }
        };
        out.push(left);
        out.push(right);
    }
    out
}

/// Decode every sample of a FLAC back to interleaved `i16`.
pub(crate) fn decode_interleaved(path: &Path) -> Vec<i16> {
    let mut out = Vec::new();
    crate::for_each_flac_block(path, |block| {
        for i in 0..block.duration() as usize {
            for ch in 0..CHANNELS {
                out.push(block.channel(ch as u32)[i] as i16);
            }
        }
        Ok(())
    })
    .expect("decode flac");
    out
}
