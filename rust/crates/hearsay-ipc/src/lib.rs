//! Binary media-frame codec for the helper<->core IPC.
//!
//! Mirror of `shared/protocol/ipc.md`; the Python `hearsay.helper.protocol` and the Swift
//! `HearsayIPC.FrameCodec` implement the same wire format. `shared/fixtures/frames.jsonl` pins the
//! contract for all three (validated by the `golden_fixtures` integration test).
//!
//! Every media message is a fixed 28-byte little-endian header followed by `n_samples *
//! bytes_per_sample` payload bytes (payload only for `audio` frames).
//!
//! The NDJSON control channel (commands / replies / events) lives in [`control`].

pub mod control;

pub use control::{
    parse_command, parse_message, to_line, Command, ControlError, Event, Inbound, JsonObj, Reply,
    ReplyError,
};

/// Frame header magic byte.
pub const MAGIC: u8 = 0xA7;
/// Protocol version.
pub const VERSION: u8 = 1;
/// Fixed header size in bytes.
pub const HEADER_SIZE: usize = 28;

/// Frame kind. Wire codes: `audio`=0, `hello`=1, `heartbeat`=2, `eos`=3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Audio,
    Hello,
    Heartbeat,
    Eos,
}

impl FrameType {
    /// Wire code for this frame type.
    pub fn to_code(self) -> u8 {
        match self {
            FrameType::Audio => 0,
            FrameType::Hello => 1,
            FrameType::Heartbeat => 2,
            FrameType::Eos => 3,
        }
    }

    /// Parse a wire code, or [`ProtocolError::UnknownFrameType`].
    pub fn from_code(code: u8) -> Result<Self, ProtocolError> {
        match code {
            0 => Ok(FrameType::Audio),
            1 => Ok(FrameType::Hello),
            2 => Ok(FrameType::Heartbeat),
            3 => Ok(FrameType::Eos),
            other => Err(ProtocolError::UnknownFrameType(other)),
        }
    }

    /// JSON/DB string form (matches the Python `StrEnum` value).
    pub fn as_str(self) -> &'static str {
        match self {
            FrameType::Audio => "audio",
            FrameType::Hello => "hello",
            FrameType::Heartbeat => "heartbeat",
            FrameType::Eos => "eos",
        }
    }
}

/// Capture channel. Wire codes: `me`=0 (mic), `them`=1 (system audio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Me,
    Them,
}

impl Stream {
    /// Wire code for this stream.
    pub fn to_code(self) -> u8 {
        match self {
            Stream::Me => 0,
            Stream::Them => 1,
        }
    }

    /// Parse a wire code, or [`ProtocolError::UnknownStream`].
    pub fn from_code(code: u8) -> Result<Self, ProtocolError> {
        match code {
            0 => Ok(Stream::Me),
            1 => Ok(Stream::Them),
            other => Err(ProtocolError::UnknownStream(other)),
        }
    }

    /// JSON/DB string form (matches the Python `StrEnum` value).
    pub fn as_str(self) -> &'static str {
        match self {
            Stream::Me => "me",
            Stream::Them => "them",
        }
    }
}

/// PCM sample format. Wire codes: `int16`=0, `float32`=1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    Int16,
    Float32,
}

impl SampleFormat {
    /// Wire code for this format.
    pub fn to_code(self) -> u8 {
        match self {
            SampleFormat::Int16 => 0,
            SampleFormat::Float32 => 1,
        }
    }

    /// Parse a wire code, or [`ProtocolError::UnknownSampleFormat`].
    pub fn from_code(code: u8) -> Result<Self, ProtocolError> {
        match code {
            0 => Ok(SampleFormat::Int16),
            1 => Ok(SampleFormat::Float32),
            other => Err(ProtocolError::UnknownSampleFormat(other)),
        }
    }

    /// Bytes per mono sample.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::Int16 => 2,
            SampleFormat::Float32 => 4,
        }
    }

    /// JSON/DB string form (matches the Python `StrEnum` value).
    pub fn as_str(self) -> &'static str {
        match self {
            SampleFormat::Int16 => "int16",
            SampleFormat::Float32 => "float32",
        }
    }
}

/// A decoded media frame. `n_samples` is derived from the payload (see [`MediaFrame::n_samples`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFrame {
    pub frame_type: FrameType,
    pub stream: Stream,
    pub format: SampleFormat,
    pub seq: u32,
    pub host_ts: u64,
    pub payload: Vec<u8>,
    pub flags: u8,
}

impl MediaFrame {
    /// Mono sample count in the payload (0 for non-audio frames).
    pub fn n_samples(&self) -> u32 {
        if self.frame_type != FrameType::Audio {
            return 0;
        }
        (self.payload.len() / self.format.bytes_per_sample()) as u32
    }
}

/// A byte sequence that does not conform to the IPC frame contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    BufferTooShort,
    BadMagic(u8),
    UnsupportedVersion(u8),
    UnknownFrameType(u8),
    UnknownStream(u8),
    UnknownSampleFormat(u8),
    AudioPayloadNotWholeSamples,
    NonAudioPayload(FrameType),
    TruncatedPayload,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::BufferTooShort => write!(f, "buffer shorter than header"),
            ProtocolError::BadMagic(b) => write!(f, "bad magic 0x{b:02x}"),
            ProtocolError::UnsupportedVersion(v) => write!(f, "unsupported version {v}"),
            ProtocolError::UnknownFrameType(c) => write!(f, "unknown frame type code {c}"),
            ProtocolError::UnknownStream(c) => write!(f, "unknown stream code {c}"),
            ProtocolError::UnknownSampleFormat(c) => write!(f, "unknown sample format code {c}"),
            ProtocolError::AudioPayloadNotWholeSamples => {
                write!(f, "audio payload length not a whole number of samples")
            }
            ProtocolError::NonAudioPayload(t) => {
                write!(f, "{} frame must not carry a payload", t.as_str())
            }
            ProtocolError::TruncatedPayload => write!(f, "truncated payload"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Serialize a frame to header+payload bytes.
pub fn encode(frame: &MediaFrame) -> Result<Vec<u8>, ProtocolError> {
    if frame.frame_type == FrameType::Audio {
        if !frame
            .payload
            .len()
            .is_multiple_of(frame.format.bytes_per_sample())
        {
            return Err(ProtocolError::AudioPayloadNotWholeSamples);
        }
    } else if !frame.payload.is_empty() {
        return Err(ProtocolError::NonAudioPayload(frame.frame_type));
    }
    let mut buf = Vec::with_capacity(HEADER_SIZE + frame.payload.len());
    buf.push(MAGIC);
    buf.push(VERSION);
    buf.push(frame.frame_type.to_code());
    buf.push(frame.stream.to_code());
    buf.push(frame.format.to_code());
    buf.push(frame.flags);
    buf.extend_from_slice(&0u16.to_le_bytes()); // reserved0
    buf.extend_from_slice(&frame.seq.to_le_bytes());
    buf.extend_from_slice(&frame.host_ts.to_le_bytes());
    buf.extend_from_slice(&frame.n_samples().to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // reserved1
    buf.extend_from_slice(&frame.payload);
    Ok(buf)
}

/// Decode exactly one frame from the front of `buf`.
pub fn decode(buf: &[u8]) -> Result<MediaFrame, ProtocolError> {
    if buf.len() < HEADER_SIZE {
        return Err(ProtocolError::BufferTooShort);
    }
    if buf[0] != MAGIC {
        return Err(ProtocolError::BadMagic(buf[0]));
    }
    if buf[1] != VERSION {
        return Err(ProtocolError::UnsupportedVersion(buf[1]));
    }
    let frame_type = FrameType::from_code(buf[2])?;
    let stream = Stream::from_code(buf[3])?;
    let format = SampleFormat::from_code(buf[4])?;
    let flags = buf[5];
    let seq = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let host_ts = u64::from_le_bytes(buf[12..20].try_into().unwrap());
    let n_samples = u32::from_le_bytes(buf[20..24].try_into().unwrap());
    let payload_len = if frame_type == FrameType::Audio {
        n_samples as usize * format.bytes_per_sample()
    } else {
        0
    };
    let payload = buf
        .get(HEADER_SIZE..HEADER_SIZE + payload_len)
        .ok_or(ProtocolError::TruncatedPayload)?
        .to_vec();
    Ok(MediaFrame {
        frame_type,
        stream,
        format,
        seq,
        host_ts,
        payload,
        flags,
    })
}

/// Payload byte count that follows a 28-byte header (0 for non-audio frames).
///
/// Lets a stream reader size the second read without decoding the whole frame.
pub fn expected_payload_len(header: &[u8]) -> Result<usize, ProtocolError> {
    if header.len() < HEADER_SIZE {
        return Err(ProtocolError::BufferTooShort);
    }
    let frame_type = FrameType::from_code(header[2])?;
    let format = SampleFormat::from_code(header[4])?;
    let n_samples = u32::from_le_bytes(header[20..24].try_into().unwrap());
    Ok(if frame_type == FrameType::Audio {
        n_samples as usize * format.bytes_per_sample()
    } else {
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello_me() -> MediaFrame {
        MediaFrame {
            frame_type: FrameType::Hello,
            stream: Stream::Me,
            format: SampleFormat::Int16,
            seq: 0,
            host_ts: 0,
            payload: vec![],
            flags: 0,
        }
    }

    #[test]
    fn audio_roundtrip_and_sample_count() {
        let frame = MediaFrame {
            frame_type: FrameType::Audio,
            stream: Stream::Them,
            format: SampleFormat::Int16,
            seq: 3,
            host_ts: 42,
            payload: vec![0, 0, 1, 0],
            flags: 0,
        };
        assert_eq!(frame.n_samples(), 2);
        assert_eq!(decode(&encode(&frame).unwrap()).unwrap(), frame);
    }

    #[test]
    fn bad_magic() {
        let mut bytes = encode(&hello_me()).unwrap();
        bytes[0] = 0x00;
        assert_eq!(decode(&bytes), Err(ProtocolError::BadMagic(0x00)));
    }

    #[test]
    fn unsupported_version() {
        let mut bytes = encode(&hello_me()).unwrap();
        bytes[1] = 2;
        assert_eq!(decode(&bytes), Err(ProtocolError::UnsupportedVersion(2)));
    }

    #[test]
    fn buffer_too_short() {
        assert_eq!(decode(&[0xA7, 1, 1]), Err(ProtocolError::BufferTooShort));
    }

    #[test]
    fn non_audio_payload_rejected() {
        let mut frame = hello_me();
        frame.payload = vec![1, 2];
        assert_eq!(
            encode(&frame),
            Err(ProtocolError::NonAudioPayload(FrameType::Hello))
        );
    }

    #[test]
    fn audio_payload_not_whole_samples() {
        let frame = MediaFrame {
            frame_type: FrameType::Audio,
            stream: Stream::Me,
            format: SampleFormat::Float32,
            seq: 0,
            host_ts: 0,
            payload: vec![1, 2, 3],
            flags: 0,
        };
        assert_eq!(
            encode(&frame),
            Err(ProtocolError::AudioPayloadNotWholeSamples)
        );
    }

    #[test]
    fn truncated_payload() {
        let mut bytes = vec![MAGIC, VERSION, 0, 1, 0, 0];
        bytes.extend_from_slice(&0u16.to_le_bytes()); // reserved0
        bytes.extend_from_slice(&0u32.to_le_bytes()); // seq
        bytes.extend_from_slice(&0u64.to_le_bytes()); // host_ts
        bytes.extend_from_slice(&4u32.to_le_bytes()); // n_samples = 4, but no payload follows
        bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved1
        assert_eq!(decode(&bytes), Err(ProtocolError::TruncatedPayload));
    }

    #[test]
    fn expected_payload_len_audio_and_non_audio() {
        let audio = encode(&MediaFrame {
            frame_type: FrameType::Audio,
            stream: Stream::Them,
            format: SampleFormat::Int16,
            seq: 0,
            host_ts: 0,
            payload: vec![0, 0, 1, 0],
            flags: 0,
        })
        .unwrap();
        assert_eq!(expected_payload_len(&audio[..HEADER_SIZE]).unwrap(), 4);
        let hello = encode(&hello_me()).unwrap();
        assert_eq!(expected_payload_len(&hello[..HEADER_SIZE]).unwrap(), 0);
    }
}
