#![cfg(feature = "sherpa")]
//! Opt-in streaming-ASR check: transcribe the JFK clip with the 20M streaming zipformer and confirm
//! the known words come through. Ignored by default (needs the model + clip). Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference streaming -- --ignored --nocapture

mod common;
use common::repo;

use hearsay_inference::{read_wav_mono_16k, StreamEventKind, StreamingAsr, StreamingModel};

fn load_asr() -> StreamingAsr {
    let dir = repo("outputs/models/sherpa/sherpa-onnx-streaming-zipformer-en-2023-06-21");
    StreamingAsr::load(StreamingModel {
        encoder: &dir.join("encoder-epoch-99-avg-1.int8.onnx"),
        decoder: &dir.join("decoder-epoch-99-avg-1.int8.onnx"),
        joiner: &dir.join("joiner-epoch-99-avg-1.int8.onnx"),
        tokens: &dir.join("tokens.txt"),
    })
    .expect("load streaming asr")
}

#[test]
#[ignore = "needs the streaming zipformer model + jfk.wav"]
fn streams_jfk_clip() {
    let asr = load_asr();
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

/// Drive the live session by feeding the clip in 0.5 s chunks: partials should grow, and a final
/// (from `finish`, or an endpoint if there is a pause) should carry the transcript.
#[test]
#[ignore = "needs the streaming zipformer model + jfk.wav"]
fn streams_jfk_in_chunks_emits_partials_then_final() {
    let asr = load_asr();
    let samples = read_wav_mono_16k(repo("outputs/jfk.wav")).expect("read jfk.wav");

    let mut session = asr.session();
    let mut partials = 0usize;
    let mut finals: Vec<String> = Vec::new();
    let chunk = 8_000; // 0.5 s at 16 kHz
    for block in samples.chunks(chunk) {
        for event in session.feed(block) {
            match event.kind {
                StreamEventKind::Partial => partials += 1,
                StreamEventKind::Final => finals.push(event.text),
            }
        }
    }
    finals.extend(session.finish().into_iter().map(|e| e.text));

    let joined = finals.join(" ").to_uppercase();
    eprintln!("partials={partials} finals={} -> {joined}", finals.len());
    assert!(partials > 0, "expected growing partials while feeding");
    assert!(
        !finals.is_empty(),
        "expected at least one finalized utterance"
    );
    assert!(
        joined.contains("COUNTRY"),
        "expected the transcript in a final: {joined}"
    );
}
