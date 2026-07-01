//! Binary media-frame codec (28-byte little-endian header) + NDJSON control protocol.
//!
//! Byte-for-byte with `shared/protocol/ipc.md` and validated against
//! `shared/fixtures/frames.jsonl`. Rust port of the Python `hearsay.helper.protocol` and the Swift
//! `HearsayIPC.FrameCodec`; all three must agree on the wire format.
//!
//! Scaffold: see `docs/architecture-cross-platform.md`.
