//! Lossless FLAC archival for a meeting's recorded audio.
//!
//! The recorder writes one uncompressed stereo `audio.wav` per meeting (Me = left, Them = right,
//! 16 kHz 16-bit — 230 MB per hour), and nothing ever shrinks it. This crate converts that WAV to
//! FLAC once the meeting is old enough, which is ~3x smaller on real meeting audio and, being
//! lossless, is invisible to everything downstream: the offline refine, re-diarization, voiceprints,
//! and playback all see the same samples back.
//!
//! Because compression deletes the user's only copy of the audio, [`compress_meeting_audio`] never
//! removes the WAV until the encoded FLAC has been decoded back and compared sample-for-sample
//! against it. Every failure path leaves the WAV byte-identical.
//!
//! WAV remains the live capture format — this runs strictly after a meeting is finalized.

use std::path::{Path, PathBuf};

#[cfg(test)]
pub(crate) mod test_support;

pub mod decode;
pub mod encode;
pub mod error;
pub mod verify;

pub use decode::{flac_channels, for_each_flac_block, read_flac_channel_16k, read_flac_mono_16k};
pub use encode::encode_wav_to_flac;
pub use error::AudioError;
pub use verify::verify_flac_matches_wav;

/// Contract-fixed capture sample rate (Hz).
pub const SAMPLE_RATE: u32 = 16_000;
/// Me = left, Them = right.
pub const CHANNELS: usize = 2;

/// The uncompressed recording a meeting folder holds while it is fresh.
pub const AUDIO_WAV: &str = "audio.wav";
/// The losslessly compressed recording it holds once archived.
pub const AUDIO_FLAC: &str = "audio.flac";
/// The in-progress encode. Named alongside the final file so the rename is same-filesystem.
const AUDIO_FLAC_PART: &str = "audio.flac.part";

/// What [`compress_meeting_audio`] reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compressed {
    /// Size of the WAV that was removed.
    pub wav_bytes: u64,
    /// Size of the FLAC that replaced it.
    pub flac_bytes: u64,
}

/// This meeting's recorded audio, whichever form it is in, or `None` when nothing was recorded.
///
/// Prefers the WAV: it is the original, it needs no decode, and both files coexist only in the
/// window between the rename and the unlink in [`compress_meeting_audio`] (or after a crash inside
/// that window, which the next sweep resolves).
pub fn resolve_recorded_audio(dir: &Path) -> Option<PathBuf> {
    let wav = dir.join(AUDIO_WAV);
    if wav.is_file() {
        return Some(wav);
    }
    let flac = dir.join(AUDIO_FLAC);
    if flac.is_file() {
        return Some(flac);
    }
    None
}

/// Replace `dir/audio.wav` with a losslessly equivalent `dir/audio.flac`.
///
/// The order matters — encode to a temporary, prove it decodes back to the source, rename
/// it into place, and only then unlink the WAV. An error at any earlier step removes the temporary
/// and leaves the WAV untouched, so a failure costs disk space, never audio. A crash between the
/// rename and the unlink leaves both files, which [`resolve_recorded_audio`] handles and the next
/// sweep cleans up.
pub fn compress_meeting_audio(dir: &Path) -> Result<Compressed, AudioError> {
    let wav = dir.join(AUDIO_WAV);
    let flac = dir.join(AUDIO_FLAC);
    let part = dir.join(AUDIO_FLAC_PART);

    let wav_bytes = std::fs::metadata(&wav)?.len();
    // A leftover from a killed encode; the encode below would truncate it anyway, but clearing it
    // first keeps a failure from leaving a stale file that looks like progress.
    if part.exists() {
        let _ = std::fs::remove_file(&part);
    }

    let result = encode_wav_to_flac(&wav, &part)
        .and_then(|_| verify_flac_matches_wav(&wav, &part))
        .and_then(|()| Ok(std::fs::metadata(&part)?.len()));
    let flac_bytes = match result {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            return Err(err);
        }
    };

    std::fs::rename(&part, &flac)?;
    std::fs::remove_file(&wav)?;
    Ok(Compressed {
        wav_bytes,
        flac_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{interleaved_pattern, stereo_spec, write_wav};

    fn meeting_dir(frames: usize) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        write_wav(
            &tmp.path().join(AUDIO_WAV),
            stereo_spec(),
            &interleaved_pattern(frames),
        );
        tmp
    }

    #[test]
    fn compressing_replaces_the_wav_with_a_smaller_flac() {
        let tmp = meeting_dir(24_000);
        let before = std::fs::read(tmp.path().join(AUDIO_WAV)).unwrap();

        let out = compress_meeting_audio(tmp.path()).expect("compress");

        assert!(!tmp.path().join(AUDIO_WAV).exists(), "wav should be gone");
        assert!(tmp.path().join(AUDIO_FLAC).is_file(), "flac should exist");
        assert!(!tmp.path().join(AUDIO_FLAC_PART).exists(), "part remained");
        assert_eq!(out.wav_bytes, before.len() as u64);
        assert_eq!(
            out.flac_bytes,
            std::fs::metadata(tmp.path().join(AUDIO_FLAC))
                .unwrap()
                .len()
        );

        // And the audio survived intact.
        let decoded = crate::test_support::decode_interleaved(&tmp.path().join(AUDIO_FLAC));
        assert_eq!(decoded, interleaved_pattern(24_000));
    }

    #[test]
    fn a_failed_compress_leaves_the_wav_untouched() {
        // A mono wav is not the recorder format, so the encode refuses it — standing in for any
        // failure before the rename. The audio must survive exactly as it was.
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join(AUDIO_WAV);
        let mono = hound::WavSpec {
            channels: 1,
            ..stereo_spec()
        };
        write_wav(&wav, mono, &interleaved_pattern(2_000));
        let before = std::fs::read(&wav).unwrap();

        assert!(compress_meeting_audio(tmp.path()).is_err());

        assert_eq!(std::fs::read(&wav).unwrap(), before, "wav was modified");
        assert!(
            !tmp.path().join(AUDIO_FLAC).exists(),
            "flac was left behind"
        );
        assert!(!tmp.path().join(AUDIO_FLAC_PART).exists(), "part remained");
    }

    #[test]
    fn a_stale_part_from_a_killed_encode_is_replaced() {
        let tmp = meeting_dir(8_000);
        std::fs::write(tmp.path().join(AUDIO_FLAC_PART), b"garbage from a crash").unwrap();

        compress_meeting_audio(tmp.path()).expect("compress");

        assert!(!tmp.path().join(AUDIO_FLAC_PART).exists());
        let decoded = crate::test_support::decode_interleaved(&tmp.path().join(AUDIO_FLAC));
        assert_eq!(decoded, interleaved_pattern(8_000));
    }

    #[test]
    fn compressing_reports_a_missing_recording() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            compress_meeting_audio(tmp.path()),
            Err(AudioError::Io(_))
        ));
    }

    #[test]
    fn resolve_prefers_the_wav_and_falls_back_to_the_flac() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(resolve_recorded_audio(tmp.path()), None);

        let flac = tmp.path().join(AUDIO_FLAC);
        std::fs::write(&flac, b"").unwrap();
        assert_eq!(resolve_recorded_audio(tmp.path()), Some(flac.clone()));

        // Both exist only in the crash window between the rename and the unlink; the original wins.
        let wav = tmp.path().join(AUDIO_WAV);
        std::fs::write(&wav, b"").unwrap();
        assert_eq!(resolve_recorded_audio(tmp.path()), Some(wav));
    }
}
