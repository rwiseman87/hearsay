//! Opt-in streaming-ASR check: transcribe the JFK clip with the 20M streaming zipformer and confirm
//! the known words come through. Ignored by default (needs the model + clip). Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference streaming -- --ignored --nocapture

use std::path::PathBuf;

use hearsay_inference::{read_wav_mono_16k, StreamingAsr, StreamingModel};

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

#[test]
#[ignore = "needs the streaming zipformer model + jfk.wav"]
fn streams_jfk_clip() {
    let dir = repo("outputs/models/sherpa-onnx-streaming-zipformer-en-20M-2023-02-17");
    let asr = StreamingAsr::load(StreamingModel {
        encoder: &dir.join("encoder-epoch-99-avg-1.int8.onnx"),
        decoder: &dir.join("decoder-epoch-99-avg-1.int8.onnx"),
        joiner: &dir.join("joiner-epoch-99-avg-1.int8.onnx"),
        tokens: &dir.join("tokens.txt"),
    })
    .expect("load streaming asr");

    let samples = read_wav_mono_16k(repo("outputs/jfk.wav")).expect("read jfk.wav");
    let text = asr.transcribe(&samples).to_uppercase();
    eprintln!("transcript: {text}");

    // The JFK line: "...ask not what your country can do for you..."
    for word in ["ASK", "COUNTRY", "FOR", "YOU"] {
        assert!(
            text.contains(word),
            "expected {word:?} in transcript: {text}"
        );
    }
}
