#![cfg(feature = "sherpa")]
//! What the consolidation pass is worth measured where it actually matters — the finished
//! transcript, not the diarizer's cluster count. Runs the full refine (whisper + `SherpaDiarizer`)
//! over one recording three times and reports the speakers that survive to the segments.
//!
//! Worth re-running whenever the consolidation rules change; the numbers below are the measured
//! baseline on a 371 s known-2-speaker recording (`small.en`), and total words must not move —
//! consolidation relabels text, it never deletes any:
//!
//! ```text
//! raw (no consolidation)                  6 speakers, 33 segments, 1082 words
//! flat 0.65 bar, no non-speech drop       4 speakers, 28 segments, 1082 words
//! evidence-scaled bar + non-speech drop   2 speakers, 26 segments, 1082 words
//! ```
//!
//! Ignored by default (needs a recording + a whisper model + the sherpa ONNX models):
//!   HEARSAY_PROBE_WAV=... HEARSAY_PROBE_MODEL=... cargo test -p hearsay-inference \
//!     --features sherpa --test refine_gate_probe -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::PathBuf;

use hearsay_attribution::ConsolidateConfig;
use hearsay_inference::{read_them_channel, DiarizeTuning, Diarizer, SherpaDiarizer, WhisperAsr};

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

#[test]
#[ignore = "measurement; needs a recording + whisper + ONNX models"]
fn refine_gate_probe() {
    let wav = PathBuf::from(std::env::var("HEARSAY_PROBE_WAV").expect("set HEARSAY_PROBE_WAV"));
    let model =
        PathBuf::from(std::env::var("HEARSAY_PROBE_MODEL").expect("set HEARSAY_PROBE_MODEL"));
    let seg = repo("outputs/models/sherpa/sherpa-onnx-pyannote-segmentation-3-0/model.onnx");
    let emb = repo("outputs/models/sherpa/nemo_en_titanet_small.onnx");

    let them = read_them_channel(&wav).expect("read Them channel");
    let asr = WhisperAsr::load(&model).expect("load whisper");

    let settings: [(&str, ConsolidateConfig); 3] = [
        (
            "raw (no consolidation)",
            ConsolidateConfig {
                merge_threshold: 2.0,
                evidence_s: 0.0,
                non_voice_ceiling: f64::NEG_INFINITY,
            },
        ),
        (
            "flat 0.65 bar, no non-speech drop",
            ConsolidateConfig {
                merge_threshold: 0.65,
                evidence_s: 0.0,
                non_voice_ceiling: f64::NEG_INFINITY,
            },
        ),
        (
            "evidence-scaled bar + non-speech drop",
            ConsolidateConfig::default(),
        ),
    ];

    for (label, consolidate) in settings {
        let diarizer = SherpaDiarizer::load_tuned(
            &seg,
            &emb,
            DiarizeTuning {
                consolidate,
                ..Default::default()
            },
        )
        .expect("load diarizer");

        let clusters = diarizer.diarize(&them).expect("diarize").turns.len();
        let out = hearsay_inference::refine_them_with(&asr, &diarizer, &them).expect("refine");

        // What the transcript actually shows, after the refine drops speakers that won no segment.
        let mut per_speaker: BTreeMap<i64, (usize, usize, f64)> = BTreeMap::new();
        for segment in &out.segments {
            let entry = per_speaker.entry(segment.ordinal).or_insert((0, 0, 0.0));
            entry.0 += 1;
            entry.1 += segment.text.split_whitespace().count();
            entry.2 += segment.end_s - segment.start_s;
        }
        let words: usize = per_speaker.values().map(|(_, words, _)| words).sum();
        eprintln!(
            "\n== {label} ==\n  {} diarizer turns -> {} transcript speakers, {} segments, {words} words",
            clusters,
            per_speaker.len(),
            out.segments.len()
        );
        for (ordinal, (segments, words, secs)) in &per_speaker {
            eprintln!("  Speaker {ordinal}: {segments} segments, {words} words, {secs:.1}s");
        }
    }
}
