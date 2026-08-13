//! Frame-at-a-time FLAC encoding of the canonical stereo meeting WAV.
//!
//! **Why not `flacenc::encode_with_fixed_block_size`?** It materializes the whole `Stream` — every
//! `Frame`, every `Residual` as `i32` vectors — before a byte can be written, which is on the order
//! of a gigabyte for a long meeting. `flacenc` also exposes no `io::Write`-backed `BitSink` (the
//! only implementors are `MemSink<u8>` / `MemSink<u64>`), so the streaming shape is built here:
//! encode one frame, drain the sink to the file, clear, repeat. Peak memory is one block plus one
//! encoded frame (~48 KB) regardless of meeting length.
//!
//! The frame layout otherwise mirrors the reference encoder: a fixed 4096-sample block size
//! declared as both the min and the max, with a short final block carried by `FrameBuf`'s
//! `filled_size` (which is what the frame header's block size is read from).
//!
//! **STREAMINFO is load-bearing, not metadata.** The frame-size range is patched in after the frames
//! are written, because `flacenc` leaves it at `min = 0xFFFFFF` / `max = 0` -- an impossible range
//! that AVFoundation refuses to decode, taking out WKWebView (the app's player) and QuickTime, while
//! still reporting the right duration and no error. Decoding the file proves nothing here: claxon
//! and libFLAC ignore these fields, so a byte-exact round trip passes over a file no macOS player
//! will play. Anything written into this header needs a real player in the loop, not just a decoder.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream};
use flacenc::error::Verify;
use flacenc::source::{Fill, FrameBuf};

use crate::error::AudioError;
use crate::{CHANNELS, SAMPLE_RATE};

/// Samples per block, per channel — the de-facto FLAC default, and what the reference encoder uses
/// at every compression level for this sample rate.
const BLOCK_SIZE: usize = 4096;
/// Byte offset of STREAMINFO's `min_frame_size`: the 4-byte `fLaC` marker, a 4-byte metadata block
/// header, then 4 bytes of block sizes. `max_frame_size` follows it. Both are 24-bit big-endian and
/// are patched in once the frames have been written and their real sizes are known.
const MIN_FRAME_OFFSET: u64 = 12;

/// Encode the stereo 16 kHz `src` WAV to `dst` as FLAC, returning the stereo frame count written.
///
/// Rejects any WAV that is not exactly the recorder's format: a file we cannot reproduce
/// bit-for-bit must be left alone, never silently converted.
pub fn encode_wav_to_flac(src: &Path, dst: &Path) -> Result<u64, AudioError> {
    let mut reader =
        hound::WavReader::open(src).map_err(|e| AudioError::Wav(format!("open {src:?}: {e}")))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE
        || spec.channels as usize != CHANNELS
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(AudioError::Unsupported(format!(
            "expected {SAMPLE_RATE} Hz {CHANNELS}-channel 16-bit int wav, got {} Hz {} ch {}-bit {:?}",
            spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
        )));
    }
    // hound's `duration` is frames (samples per channel) — what FLAC calls "total samples".
    let total = reader.duration() as usize;

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| AudioError::Flac(format!("encoder config: {e:?}")))?;

    let mut stream = Stream::new(SAMPLE_RATE as usize, CHANNELS, 16)
        .map_err(|e| AudioError::Flac(format!("stream header: {e:?}")))?;
    // `total_samples` is what a browser's `<audio>` reports as `duration`, so the transcript
    // scrubber depends on this being right.
    stream.stream_info_mut().set_total_samples(total);
    stream
        .stream_info_mut()
        .set_block_sizes(BLOCK_SIZE, BLOCK_SIZE)
        .map_err(|e| AudioError::Flac(format!("block sizes: {e:?}")))?;

    let file = File::create(dst)?;
    let mut out = BufWriter::new(file);
    let mut sink = ByteSink::new();

    // A `Stream` carrying no frames writes exactly the `fLaC` marker plus the metadata blocks, so
    // this emits the header and nothing else; the frames follow one at a time below.
    stream
        .write(&mut sink)
        .map_err(|e| AudioError::Flac(format!("write header: {e:?}")))?;
    out.write_all(sink.as_slice())?;
    sink.clear();

    let mut framebuf = FrameBuf::with_size(CHANNELS, BLOCK_SIZE)
        .map_err(|e| AudioError::Flac(format!("frame buffer: {e:?}")))?;
    let mut interleaved: Vec<i32> = Vec::with_capacity(BLOCK_SIZE * CHANNELS);
    let mut samples = reader.samples::<i16>();
    let mut written: usize = 0;
    let mut frame_number: usize = 0;
    let (mut min_frame, mut max_frame) = (u32::MAX, 0u32);

    while written < total {
        let this_block = BLOCK_SIZE.min(total - written);
        interleaved.clear();
        for _ in 0..this_block * CHANNELS {
            let sample = samples
                .next()
                .ok_or_else(|| AudioError::Wav("wav ended before its declared length".into()))?
                .map_err(|e| AudioError::Wav(format!("read sample: {e}")))?;
            interleaved.push(sample as i32);
        }
        // A short final block needs no resize: `fill_interleaved` records `filled_size`, and that is
        // what the frame header's block size is taken from.
        framebuf
            .fill_interleaved(&interleaved)
            .map_err(|e| AudioError::Flac(format!("fill frame: {e:?}")))?;

        let frame = flacenc::encode_fixed_size_frame(
            &config,
            &framebuf,
            frame_number,
            stream.stream_info(),
        )
        .map_err(|e| AudioError::Flac(format!("encode frame {frame_number}: {e:?}")))?;
        // `Frame::write` byte-aligns before its CRC-16 footer, so a fresh sink per frame
        // concatenates into a valid stream.
        frame
            .write(&mut sink)
            .map_err(|e| AudioError::Flac(format!("write frame {frame_number}: {e:?}")))?;
        let frame_bytes = sink.as_slice().len() as u32;
        min_frame = min_frame.min(frame_bytes);
        max_frame = max_frame.max(frame_bytes);
        out.write_all(sink.as_slice())?;
        sink.clear();

        written += this_block;
        frame_number += 1;
    }

    out.flush()?;
    let mut file = out
        .into_inner()
        .map_err(|e| AudioError::Io(e.into_error()))?;

    // Patch the real frame-size range into STREAMINFO. Leaving these at their defaults writes
    // min = 0xFFFFFF (flacenc's "no frame seen yet" sentinel) against max = 0, an impossible range
    // that some decoders -- AVFoundation among them, so every macOS player -- refuse to play even
    // though the audio decodes perfectly. A stream with no frames declares both unknown (0).
    let (min_frame, max_frame) = if frame_number == 0 {
        (0, 0)
    } else {
        (min_frame, max_frame)
    };
    let sizes = [
        (min_frame >> 16) as u8,
        (min_frame >> 8) as u8,
        min_frame as u8,
        (max_frame >> 16) as u8,
        (max_frame >> 8) as u8,
        max_frame as u8,
    ];
    file.seek(SeekFrom::Start(MIN_FRAME_OFFSET))?;
    file.write_all(&sizes)?;
    file.sync_all()?;
    Ok(written as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{decode_interleaved, interleaved_pattern, stereo_spec, write_wav};

    /// Encode `frames` of the test pattern and assert the decode is sample-identical. This is the
    /// property the whole archival feature rests on: if it ever fails, compression is destroying
    /// audio.
    fn assert_lossless_round_trip(frames: usize) {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("audio.wav");
        let flac = tmp.path().join("audio.flac");
        let samples = interleaved_pattern(frames);
        write_wav(&wav, stereo_spec(), &samples);

        let written = encode_wav_to_flac(&wav, &flac).expect("encode");
        assert_eq!(written, frames as u64, "frame count for {frames} frames");

        let decoded = decode_interleaved(&flac);
        assert_eq!(
            decoded.len(),
            samples.len(),
            "sample count for {frames} frames"
        );
        assert_eq!(decoded, samples, "samples differ for {frames} frames");
    }

    #[test]
    fn round_trip_is_lossless() {
        assert_lossless_round_trip(16_000);
    }

    #[test]
    fn round_trip_is_lossless_at_block_boundaries() {
        // Exactly one block, one block plus a tail too short to stand alone, an exact multiple, a
        // short final block, and a file smaller than a single block.
        for frames in [
            BLOCK_SIZE,
            BLOCK_SIZE + 1,
            BLOCK_SIZE * 2,
            4096 * 2 + 3,
            100,
        ] {
            assert_lossless_round_trip(frames);
        }
    }

    /// STREAMINFO is not just decoration: a decoder may size its packet buffer from the frame-size
    /// range, and an impossible one (min > max, or max = 0 with a non-empty stream) makes
    /// AVFoundation load the file, report the right duration, and then refuse to play a sample.
    /// The round-trip tests cannot catch this -- claxon ignores these fields entirely.
    #[test]
    fn declares_a_coherent_frame_size_range() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("audio.wav");
        let flac = tmp.path().join("audio.flac");
        write_wav(
            &wav,
            stereo_spec(),
            &interleaved_pattern(BLOCK_SIZE * 3 + 17),
        );
        encode_wav_to_flac(&wav, &flac).expect("encode");

        let info = claxon::FlacReader::open(&flac).unwrap().streaminfo();
        let min = info
            .min_frame_size
            .expect("min_frame_size must be declared");
        let max = info
            .max_frame_size
            .expect("max_frame_size must be declared");
        assert!(
            min > 0,
            "min_frame_size must not be the empty-stream marker"
        );
        assert!(min <= max, "impossible range: min {min} > max {max}");
        // And it must actually bound the frames: the file is header + frames, so no frame can be
        // larger than the file.
        assert!((max as u64) < std::fs::metadata(&flac).unwrap().len());
    }

    #[test]
    fn declares_total_samples_so_players_report_a_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("audio.wav");
        let flac = tmp.path().join("audio.flac");
        let frames = BLOCK_SIZE * 2 + 7;
        write_wav(&wav, stereo_spec(), &interleaved_pattern(frames));
        encode_wav_to_flac(&wav, &flac).expect("encode");

        let reader = claxon::FlacReader::open(&flac).expect("open flac");
        let info = reader.streaminfo();
        assert_eq!(info.samples, Some(frames as u64));
        assert_eq!(info.sample_rate, SAMPLE_RATE);
        assert_eq!(info.channels as usize, CHANNELS);
        assert_eq!(info.bits_per_sample, 16);
    }

    #[test]
    fn compresses_speech_like_audio() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("audio.wav");
        let flac = tmp.path().join("audio.flac");
        // Mostly-quiet with a tone, which is what a meeting looks like — the case the feature exists
        // for. (The full test pattern is deliberately noisy and compresses far less.)
        let frames = 32_000;
        let samples: Vec<i16> = (0..frames * CHANNELS)
            .map(|i| ((i as f32 * 0.01).sin() * 800.0) as i16)
            .collect();
        write_wav(&wav, stereo_spec(), &samples);
        encode_wav_to_flac(&wav, &flac).expect("encode");

        let wav_len = std::fs::metadata(&wav).unwrap().len();
        let flac_len = std::fs::metadata(&flac).unwrap().len();
        assert!(
            flac_len < wav_len / 2,
            "expected a real saving, got {flac_len} from {wav_len}"
        );
    }

    #[test]
    fn rejects_wavs_that_are_not_the_recorder_format() {
        let tmp = tempfile::tempdir().unwrap();
        let flac = tmp.path().join("audio.flac");

        let mono = hound::WavSpec {
            channels: 1,
            ..stereo_spec()
        };
        let wrong_rate = hound::WavSpec {
            sample_rate: 44_100,
            ..stereo_spec()
        };
        let float = hound::WavSpec {
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
            ..stereo_spec()
        };

        for (name, spec) in [("mono", mono), ("rate", wrong_rate)] {
            let wav = tmp.path().join(format!("{name}.wav"));
            write_wav(&wav, spec, &interleaved_pattern(64));
            let before = std::fs::read(&wav).unwrap();
            assert!(
                matches!(
                    encode_wav_to_flac(&wav, &flac),
                    Err(AudioError::Unsupported(_))
                ),
                "{name} should be rejected"
            );
            assert_eq!(std::fs::read(&wav).unwrap(), before, "{name} wav modified");
        }

        // A float WAV needs float samples, so it is written separately.
        let wav = tmp.path().join("float.wav");
        let mut writer = hound::WavWriter::create(&wav, float).unwrap();
        for _ in 0..128 {
            writer.write_sample(0.25f32).unwrap();
        }
        writer.finalize().unwrap();
        assert!(matches!(
            encode_wav_to_flac(&wav, &flac),
            Err(AudioError::Unsupported(_))
        ));
    }
}
