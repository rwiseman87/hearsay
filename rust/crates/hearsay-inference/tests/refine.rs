//! Opt-in refine check against a real recorded meeting: re-diarize the Them track (Swift
//! `hearsay-diarize`) + re-transcribe each turn with whisper. Ignored by default (needs a recording
//! + the built sidecar + a model); run with:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --features metal -- --ignored --nocapture

mod common;
use common::repo;

use std::collections::BTreeSet;
use std::time::Duration;

use hearsay_inference::{read_them_channel, refine_them, WhisperAsr};

#[test]
#[ignore = "needs a recorded meeting + hearsay-diarize + a whisper model"]
fn refines_real_meeting_them_track() {
    let audio = repo("outputs/recordings/2026-07-01_1833_miguel-kristina-test2/audio.wav");
    let them = read_them_channel(&audio).expect("read Them channel");
    let asr = WhisperAsr::load(repo("outputs/models/ggml-large-v3-turbo.bin")).expect("load model");
    let diarize = repo("helper/.build/arm64-apple-macosx/debug/hearsay-diarize");

    let output = refine_them(&asr, &diarize, &them, Duration::from_secs(600)).expect("refine");
    let segments = &output.segments;

    let speakers: BTreeSet<i64> = segments.iter().map(|s| s.ordinal).collect();
    eprintln!(
        "{} refined segments, {} speakers, {} voiceprints",
        segments.len(),
        speakers.len(),
        output.centroids.len()
    );
    for seg in segments.iter().take(8) {
        eprintln!(
            "  Speaker {} [{:.1}-{:.1}] {}",
            seg.ordinal, seg.start_s, seg.end_s, seg.text
        );
    }
    assert!(!segments.is_empty(), "expected refined segments");
    assert!(
        speakers.len() >= 2,
        "expected >= 2 speakers, got {}",
        speakers.len()
    );
    // Whole-track attribution keeps a voiceprint only for speakers that appear in the segments (a
    // speaker may also lack an embedding), so voiceprints never exceed attributed speakers, and each
    // belongs to one of them.
    assert!(
        !output.centroids.is_empty() && output.centroids.len() <= speakers.len(),
        "expected 1..={} voiceprints, got {}",
        speakers.len(),
        output.centroids.len()
    );
    for ordinal in output.centroids.keys() {
        assert!(
            speakers.contains(ordinal),
            "voiceprint for unattributed speaker {ordinal}"
        );
    }
}
