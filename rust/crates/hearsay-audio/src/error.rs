//! Audio archival error type.

/// A failure encoding, decoding, or verifying a meeting's recorded audio.
#[derive(Debug)]
pub enum AudioError {
    /// An underlying I/O error.
    Io(std::io::Error),
    /// A WAV read/parse failure.
    Wav(String),
    /// A FLAC encode/decode failure.
    Flac(String),
    /// The encoded FLAC does not decode back to the source samples — the WAV is left untouched.
    Mismatch(String),
    /// The file is not the canonical recording format this crate archives.
    Unsupported(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::Io(e) => write!(f, "io error: {e}"),
            AudioError::Wav(m) => write!(f, "wav error: {m}"),
            AudioError::Flac(m) => write!(f, "flac error: {m}"),
            AudioError::Mismatch(m) => write!(f, "flac verification failed: {m}"),
            AudioError::Unsupported(m) => write!(f, "unsupported audio: {m}"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(err: std::io::Error) -> Self {
        AudioError::Io(err)
    }
}
