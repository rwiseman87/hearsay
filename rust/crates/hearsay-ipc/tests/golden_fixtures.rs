//! Validates the Rust codec against the cross-language golden fixtures in
//! `shared/fixtures/frames.jsonl` — the same vectors the Swift codec checks in CI.
//!
//! For each fixture: (a) decode `encoded_hex` and assert it matches the header fields + payload,
//! then (b) re-encode and assert the bytes are exactly `encoded_hex`.

use hearsay_ipc::{decode, encode};
use std::path::PathBuf;

fn fixtures_path() -> PathBuf {
    // rust/crates/hearsay-ipc -> repo root -> shared/fixtures/frames.jsonl
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../shared/fixtures/frames.jsonl")
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd-length hex string");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[test]
fn golden_fixtures_decode_and_reencode() {
    let text = std::fs::read_to_string(fixtures_path()).expect("read frames.jsonl");
    let mut count = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: serde_json::Value = serde_json::from_str(line).expect("parse fixture line");
        let desc = rec["desc"].as_str().unwrap();
        let header = &rec["header"];
        let encoded_hex = rec["encoded_hex"].as_str().unwrap();
        let expected_payload = hex_decode(rec["payload_hex"].as_str().unwrap());

        // (a) decode encoded_hex -> matches the header fields + payload
        let frame = decode(&hex_decode(encoded_hex))
            .unwrap_or_else(|e| panic!("{desc}: decode failed: {e}"));
        assert_eq!(
            frame.frame_type.as_str(),
            header["type"].as_str().unwrap(),
            "{desc}: type"
        );
        assert_eq!(
            frame.stream.as_str(),
            header["stream"].as_str().unwrap(),
            "{desc}: stream"
        );
        assert_eq!(
            frame.format.as_str(),
            header["format"].as_str().unwrap(),
            "{desc}: format"
        );
        assert_eq!(
            u64::from(frame.seq),
            header["seq"].as_u64().unwrap(),
            "{desc}: seq"
        );
        assert_eq!(
            frame.host_ts,
            header["host_ts"].as_u64().unwrap(),
            "{desc}: host_ts"
        );
        assert_eq!(
            u64::from(frame.flags),
            header["flags"].as_u64().unwrap(),
            "{desc}: flags"
        );
        assert_eq!(
            u64::from(frame.n_samples()),
            header["n_samples"].as_u64().unwrap(),
            "{desc}: n_samples"
        );
        assert_eq!(frame.payload, expected_payload, "{desc}: payload");

        // (b) re-encode -> exactly encoded_hex
        let reencoded = encode(&frame).unwrap_or_else(|e| panic!("{desc}: encode failed: {e}"));
        assert_eq!(
            hex_encode(&reencoded),
            encoded_hex,
            "{desc}: re-encode mismatch"
        );

        count += 1;
    }
    assert!(
        count >= 5,
        "expected at least the 5 canonical fixtures, got {count}"
    );
}
