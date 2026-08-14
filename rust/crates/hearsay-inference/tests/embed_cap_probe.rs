#![cfg(feature = "sherpa")]
//! Regression: a speaker with more than ~2 minutes of speech must not crash the process.
//!
//! `SherpaDiarizer::diarize` concatenates all of a speaker's turns for their voiceprint. TitaNet
//! -small caps at 12288 encoder frames (1_966_080 samples @ 160/frame = 122.88 s); one sample over
//! and onnxruntime throws out of `mconv.3/Where_1`, the exception crosses sherpa's C API, and Rust
//! aborts the whole process — so this cannot be an `assert!`/`should_panic`, only an absence of
//! death. `embed` chunks under the cap and averages, which this exercises well past it.
//!
//! Needs the TitaNet ONNX model (`make fetch-sherpa-models`). Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --release \
//!     --features sherpa --test embed_cap_probe -- --ignored --nocapture

mod common;
use common::sherpa_model as model;

use hearsay_attribution::voiceprint::cosine;
use hearsay_inference::{Diarizer, SherpaDiarizer};

const SAMPLE_RATE: usize = 16_000;

/// Deterministic voiced-ish tone so the segmentation model finds speech and `is_ready` passes.
fn tone(secs: f64, hz: f64) -> Vec<f32> {
    let n = (secs * SAMPLE_RATE as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / SAMPLE_RATE as f64;
            let envelope = 0.3 * (1.0 + (2.0 * std::f64::consts::PI * 3.0 * t).sin());
            ((2.0 * std::f64::consts::PI * hz * t).sin() * envelope) as f32
        })
        .collect()
}

#[test]
#[ignore = "needs the TitaNet-small embedding ONNX model"]
fn diarize_survives_a_speaker_past_the_embedder_frame_cap() {
    let diarizer = SherpaDiarizer::load(
        &model("sherpa-onnx-pyannote-segmentation-3-0/model.onnx"),
        &model("nemo_en_titanet_small.onnx"),
    )
    .expect("load diarizer");

    // 300 s — 2.4x the 122.88 s cap, so the pre-fix concatenate-and-embed-once aborted here.
    let result = diarizer.diarize(&tone(300.0, 140.0)).expect("diarize");
    eprintln!(
        "{} turns, {} voiceprints",
        result.turns.len(),
        result.embeddings.len()
    );
    for (ord, vector) in &result.embeddings {
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert_eq!(vector.len(), 192, "speaker {ord} embedding dim");
        assert!(norm.is_finite() && norm > 0.0, "speaker {ord} norm {norm}");
    }
}

/// The averaged voiceprint must still identify the speaker: the same voice past the cap should stay
/// far more similar to itself than to a different one. Guards the chunk-averaging from silently
/// degrading into mush.
#[test]
#[ignore = "needs the TitaNet-small embedding ONNX model"]
fn averaged_voiceprint_still_discriminates() {
    let diarizer = SherpaDiarizer::load(
        &model("sherpa-onnx-pyannote-segmentation-3-0/model.onnx"),
        &model("nemo_en_titanet_small.onnx"),
    )
    .expect("load diarizer");

    let embed = |secs: f64, hz: f64| -> Vec<f32> {
        let result = diarizer.diarize(&tone(secs, hz)).expect("diarize");
        result
            .embeddings
            .into_values()
            .next()
            .expect("a voiceprint")
    };

    let long_a = embed(300.0, 140.0);
    let short_a = embed(60.0, 140.0);
    let long_b = embed(300.0, 240.0);

    let same = cosine(&long_a, &short_a);
    let different = cosine(&long_a, &long_b);
    eprintln!("same-source {same:.3}, cross-source {different:.3}");
    assert!(
        same > different,
        "chunk-averaged voiceprint lost its identity: same {same:.3} <= different {different:.3}"
    );
}
