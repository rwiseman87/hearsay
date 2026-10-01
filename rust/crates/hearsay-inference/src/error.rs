//! Inference error type.

/// A failure reading audio or running the refine sidecar.
#[derive(Debug)]
pub enum InferenceError {
    /// A `hearsay-diarize` sidecar failure (spawn, timeout, non-zero exit, bad output).
    Sidecar(String),
    /// An audio I/O / format problem.
    Audio(String),
    /// The sidecar found no speech in the Them track — nothing to refine (benign).
    NoSpeech,
    /// An underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferenceError::Sidecar(m) => write!(f, "sidecar error: {m}"),
            InferenceError::Audio(m) => write!(f, "audio error: {m}"),
            InferenceError::NoSpeech => write!(f, "no speech detected in the Them track"),
            InferenceError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for InferenceError {}

impl From<std::io::Error> for InferenceError {
    fn from(err: std::io::Error) -> Self {
        InferenceError::Io(err)
    }
}
