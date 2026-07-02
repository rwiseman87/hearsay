//! Inference error type.

/// A failure loading a model, reading audio, or running inference.
#[derive(Debug)]
pub enum InferenceError {
    /// A whisper.cpp model-load or transcription failure.
    Whisper(String),
    /// A sherpa-onnx diarization / speaker-embedding failure.
    Diarize(String),
    /// A sherpa-onnx streaming-ASR (online recognizer) failure.
    Streaming(String),
    /// An audio I/O / format problem.
    Audio(String),
    /// An underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferenceError::Whisper(m) => write!(f, "whisper error: {m}"),
            InferenceError::Diarize(m) => write!(f, "diarize error: {m}"),
            InferenceError::Streaming(m) => write!(f, "streaming asr error: {m}"),
            InferenceError::Audio(m) => write!(f, "audio error: {m}"),
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
