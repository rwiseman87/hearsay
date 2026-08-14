//! Regenerate the cross-language IPC golden fixtures from the canonical messages, using this crate's
//! codecs. Rust is the source of truth for the IPC contract; the Swift `hearsay-helper selftest` and
//! this crate's tests both validate against the files this writes. Wired as part of `make codegen`.
//!
//! Two files are emitted:
//! - `shared/fixtures/frames.jsonl` — canonical media frames (`desc`/`header`/`payload_hex`/`encoded_hex`).
//! - `shared/fixtures/control.jsonl` — canonical NDJSON control messages (`desc`/`kind`/`encoded`),
//!   pinning the command / ok-reply / fail-reply / every-event wire form so the Rust and Swift
//!   control codecs cannot silently drift on key ordering, slash-escaping, or number formatting.
//!
//! Emits a stable byte layout (spaced `", "` / `": "` separators + insertion-ordered keys) so the
//! fixtures never churn.

use std::path::PathBuf;

use hearsay_ipc::{
    encode, to_line, Command, Event, FrameType, JsonObj, MediaFrame, Reply, SampleFormat, Stream,
};
use serde_json::{json, Value};

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

/// One control-channel golden record: `kind` names how the validators decode `encoded` (a bare
/// NDJSON wire line, no trailing `\n`), and `desc` identifies it. Serialized in field-declaration
/// order, so the file is stable across regenerations.
#[derive(serde::Serialize)]
struct ControlFixture {
    desc: &'static str,
    kind: &'static str,
    encoded: String,
}

fn control_obj(value: Value) -> JsonObj {
    value
        .as_object()
        .cloned()
        .expect("canonical control payload is a JSON object")
}

/// Encode a control message to its exact wire line, minus the trailing `\n` the fixture omits.
fn control_line<T: serde::Serialize>(value: &T) -> String {
    let mut bytes = to_line(value).expect("encode canonical control message");
    bytes.pop(); // drop the trailing '\n'
    String::from_utf8(bytes).expect("control line is utf8")
}

fn event_fixture(desc: &'static str, event: &str, ts: u64, data: Value) -> ControlFixture {
    ControlFixture {
        desc,
        kind: "event",
        encoded: control_line(&Event {
            event: event.to_string(),
            ts,
            data: control_obj(data),
        }),
    }
}

/// Canonical control messages: a command, an ok-reply, a fail-reply, and one event per kind in
/// `shared/protocol/ipc.md`. `status` carries a `1/3` to pin slash-escaping; `level`
/// carries a float to pin number formatting.
fn canonical_control() -> Vec<ControlFixture> {
    vec![
        ControlFixture {
            desc: "command_start_capture",
            kind: "command",
            encoded: control_line(&Command {
                id: 7,
                cmd: "start_capture".to_string(),
                args: control_obj(json!({"tap_mode": "global_except_self", "sample_rate": 16000})),
            }),
        },
        ControlFixture {
            desc: "reply_ok_ping",
            kind: "reply_ok",
            encoded: control_line(&Reply::ok(1, control_obj(json!({"pong": true})))),
        },
        ControlFixture {
            desc: "reply_fail_no_permission",
            kind: "reply_fail",
            encoded: control_line(&Reply::fail(2, "no_permission", "microphone denied")),
        },
        event_fixture(
            "event_hello",
            "hello",
            0,
            json!({"helper_version": "0.1.0", "protocol_version": 1, "pid": 4242}),
        ),
        event_fixture(
            "event_status",
            "status",
            1_000_000_000,
            json!({"state": "degraded", "detail": "retrying 1/3", "device": "Built-in Microphone"}),
        ),
        event_fixture(
            "event_permission",
            "permission",
            1_000_000_001,
            json!({
                "microphone": "granted",
                "audio_capture": "granted"
            }),
        ),
        event_fixture(
            "event_tap_health",
            "tap_health",
            123_456_789,
            json!({"state": "recovered", "action": "rebuilt_tap"}),
        ),
        event_fixture(
            "event_mic_health",
            "mic_health",
            123_456_790,
            json!({"state": "recovered", "action": "restarted_engine"}),
        ),
        event_fixture(
            "event_level",
            "level",
            2_000_000_000,
            json!({"stream": "them", "rms": 0.5}),
        ),
        event_fixture(
            "event_error",
            "error",
            2_000_000_004,
            json!({"scope": "audio_capture", "message": "device removed", "fatal": false}),
        ),
    ]
}

fn main() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../shared/fixtures");

    let frames = fixtures.join("frames.jsonl");
    let frame_lines: Vec<String> = canonical_frames()
        .iter()
        .map(|(desc, frame)| record(desc, frame))
        .collect();
    std::fs::write(&frames, format!("{}\n", frame_lines.join("\n"))).expect("write frames.jsonl");
    println!(
        "wrote {} fixtures to {}",
        frame_lines.len(),
        frames.display()
    );

    let control = fixtures.join("control.jsonl");
    let control_lines: Vec<String> = canonical_control()
        .iter()
        .map(|fixture| serde_json::to_string(fixture).expect("serialize control fixture"))
        .collect();
    std::fs::write(&control, format!("{}\n", control_lines.join("\n")))
        .expect("write control.jsonl");
    println!(
        "wrote {} fixtures to {}",
        control_lines.len(),
        control.display()
    );
}
