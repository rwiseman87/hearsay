//! Regenerate the cross-language IPC golden fixtures (`shared/fixtures/frames.jsonl`) from the
//! canonical frames, using this crate's codec. Rust is the source of truth for the IPC contract;
//! the Swift `hearsay-helper selftest` and this crate's `golden_fixtures` test both validate against
//! the file this writes. Wired as part of `make codegen`.
//!
//! Ported from the retired `scripts/gen_fixtures.py`; emits the identical byte layout (Python
//! `json.dumps` default separators + insertion-ordered keys) so the fixtures never churn.

use std::path::PathBuf;

use hearsay_ipc::{encode, FrameType, MediaFrame, SampleFormat, Stream};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The canonical frames pinned by the fixtures (mirror of the retired `protocol.canonical_frames`).
fn canonical_frames() -> Vec<(&'static str, MediaFrame)> {
    let them_i16: Vec<u8> = [0i16, 1, -1, 32767]
        .into_iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let me_f32: Vec<u8> = [0.0f32, -1.0]
        .into_iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    vec![
        (
            "hello_me_int16",
            MediaFrame {
                frame_type: FrameType::Hello,
                stream: Stream::Me,
                format: SampleFormat::Int16,
                seq: 0,
                host_ts: 0,
                payload: Vec::new(),
                flags: 0,
            },
        ),
        (
            "audio_them_int16",
            MediaFrame {
                frame_type: FrameType::Audio,
                stream: Stream::Them,
                format: SampleFormat::Int16,
                seq: 0,
                host_ts: 1_000_000_000,
                payload: them_i16,
                flags: 0,
            },
        ),
        (
            "audio_me_float32",
            MediaFrame {
                frame_type: FrameType::Audio,
                stream: Stream::Me,
                format: SampleFormat::Float32,
                seq: 5,
                host_ts: 1_234_567_890_123,
                payload: me_f32,
                flags: 0,
            },
        ),
        (
            "heartbeat_them",
            MediaFrame {
                frame_type: FrameType::Heartbeat,
                stream: Stream::Them,
                format: SampleFormat::Int16,
                seq: 10,
                host_ts: 2_000_000_000,
                payload: Vec::new(),
                flags: 0,
            },
        ),
        (
            "eos_them",
            MediaFrame {
                frame_type: FrameType::Eos,
                stream: Stream::Them,
                format: SampleFormat::Int16,
                seq: 11,
                host_ts: 2_100_000_000,
                payload: Vec::new(),
                flags: 0,
            },
        ),
    ]
}

fn record(desc: &str, frame: &MediaFrame) -> String {
    let encoded = encode(frame).expect("encode canonical frame");
    let header = format!(
        "{{\"type\": \"{}\", \"stream\": \"{}\", \"format\": \"{}\", \"seq\": {}, \"host_ts\": {}, \"flags\": {}, \"n_samples\": {}}}",
        frame.frame_type.as_str(),
        frame.stream.as_str(),
        frame.format.as_str(),
        frame.seq,
        frame.host_ts,
        frame.flags,
        frame.n_samples(),
    );
    format!(
        "{{\"desc\": \"{desc}\", \"header\": {header}, \"payload_hex\": \"{}\", \"encoded_hex\": \"{}\"}}",
        hex(&frame.payload),
        hex(&encoded),
    )
}

fn main() {
    let out =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../shared/fixtures/frames.jsonl");
    let lines: Vec<String> = canonical_frames()
        .iter()
        .map(|(desc, frame)| record(desc, frame))
        .collect();
    let body = format!("{}\n", lines.join("\n"));
    std::fs::write(&out, body).expect("write frames.jsonl");
    println!("wrote {} fixtures to {}", lines.len(), out.display());
}
