//! Inference error type.

/// A failure loading a model, reading audio, or running inference.
#[derive(Debug)]
pub enum InferenceError {
    /// A whisper.cpp model-load or transcription failure.
    Whisper(String),
    /// A diarizer sidecar failure (spawn, timeout, bad output).
    Diarize(String),
    /// An audio I/O / format problem.
    Audio(String),
    /// The offline diarizer found no speech in the Them track — nothing to refine (benign).
    NoSpeech,
    /// An underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferenceError::Whisper(m) => write!(f, "whisper error: {m}"),
            InferenceError::Diarize(m) => write!(f, "diarize error: {m}"),
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
