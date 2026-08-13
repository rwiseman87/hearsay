//! Byte-exact proof that an encoded FLAC reproduces its source WAV.
//!
//! This runs before the WAV — the user's only copy of the meeting — is deleted, so it compares
//! every sample rather than trusting a checksum or the encoder. Both sides are streamed in lockstep
//! so a long meeting never materializes twice in memory. (This is also why the encoder leaves the
//! STREAMINFO MD5 signature at zero: an exact comparison is strictly stronger than a checksum.
//! Unlike the frame-size range, that field being zero is verified to play in WKWebView and
//! QuickTime, not merely assumed spec-legal -- see `encode.rs`.)

use std::path::Path;

use crate::decode::for_each_flac_block;
use crate::error::AudioError;
use crate::CHANNELS;

/// Verify that `flac` decodes to exactly the samples in `wav`. Any difference in a sample or in the
/// total length is an error.
pub fn verify_flac_matches_wav(wav: &Path, flac: &Path) -> Result<(), AudioError> {
    let mut reader =
        hound::WavReader::open(wav).map_err(|e| AudioError::Wav(format!("open {wav:?}: {e}")))?;
    let mut expected = reader.samples::<i16>();
    let mut compared: u64 = 0;

    let channels = {
        let reader = claxon::FlacReader::open(flac)
            .map_err(|e| AudioError::Flac(format!("open {flac:?}: {e}")))?;
        reader.streaminfo().channels as usize
    };
    if channels != CHANNELS {
        return Err(AudioError::Mismatch(format!(
            "expected {CHANNELS} channels in the flac, got {channels}"
        )));
    }

    for_each_flac_block(flac, |block| {
        for i in 0..block.duration() as usize {
            for ch in 0..CHANNELS {
                let actual = block.channel(ch as u32)[i];
                let want = expected
                    .next()
                    .ok_or_else(|| {
                        AudioError::Mismatch(format!(
                            "flac is longer than the wav (extra sample at frame {})",
                            compared / CHANNELS as u64
                        ))
                    })?
                    .map_err(|e| AudioError::Wav(format!("read sample: {e}")))?;
                if actual != want as i32 {
                    return Err(AudioError::Mismatch(format!(
                        "sample {compared} differs: wav {want}, flac {actual}"
                    )));
                }
                compared += 1;
            }
        }
        Ok(())
    })?;

    if expected.next().is_some() {
        return Err(AudioError::Mismatch(format!(
            "flac is shorter than the wav (stopped after {} frames)",
            compared / CHANNELS as u64
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_wav_to_flac;
    use crate::test_support::{interleaved_pattern, stereo_spec, write_wav};

    const FRAMES: usize = 6_000;

    /// A matching wav/flac pair, plus the temp dir keeping them alive.
    fn pair(frames: usize) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("audio.wav");
        let flac = tmp.path().join("audio.flac");
        write_wav(&wav, stereo_spec(), &interleaved_pattern(frames));
        encode_wav_to_flac(&wav, &flac).expect("encode");
        (tmp, wav, flac)
    }

    #[test]
    fn accepts_a_faithful_encode() {
        let (_tmp, wav, flac) = pair(FRAMES);
        verify_flac_matches_wav(&wav, &flac).expect("verify");
    }

    #[test]
    fn rejects_different_samples() {
        let (tmp, _wav, flac) = pair(FRAMES);
        // Same length, different content: the case a checksum-free encoder bug would produce.
        let other = tmp.path().join("other.wav");
        let mut samples = interleaved_pattern(FRAMES);
        samples[FRAMES] = samples[FRAMES].wrapping_add(1);
        write_wav(&other, stereo_spec(), &samples);

        assert!(matches!(
            verify_flac_matches_wav(&other, &flac),
            Err(AudioError::Mismatch(_))
        ));
    }

    #[test]
    fn rejects_a_flac_shorter_than_the_wav() {
        let (tmp, _wav, flac) = pair(FRAMES);
        let longer = tmp.path().join("longer.wav");
        write_wav(&longer, stereo_spec(), &interleaved_pattern(FRAMES + 4096));

        assert!(matches!(
            verify_flac_matches_wav(&longer, &flac),
            Err(AudioError::Mismatch(_))
        ));
    }

    #[test]
    fn rejects_a_flac_longer_than_the_wav() {
        let (tmp, _wav, flac) = pair(FRAMES);
        let shorter = tmp.path().join("shorter.wav");
        write_wav(&shorter, stereo_spec(), &interleaved_pattern(FRAMES - 4096));

        assert!(matches!(
            verify_flac_matches_wav(&shorter, &flac),
            Err(AudioError::Mismatch(_))
        ));
    }

    #[test]
    fn rejects_a_truncated_flac() {
        let (_tmp, wav, flac) = pair(FRAMES);
        let bytes = std::fs::read(&flac).unwrap();
        std::fs::write(&flac, &bytes[..bytes.len() / 2]).unwrap();

        assert!(verify_flac_matches_wav(&wav, &flac).is_err());
    }

    #[test]
    fn rejects_a_corrupted_flac() {
        let (_tmp, wav, flac) = pair(FRAMES);
        let mut bytes = std::fs::read(&flac).unwrap();
        // Well past the header, inside the frame data.
        let at = bytes.len() / 2;
        bytes[at] ^= 0xFF;
        std::fs::write(&flac, &bytes).unwrap();

        assert!(verify_flac_matches_wav(&wav, &flac).is_err());
    }
}
