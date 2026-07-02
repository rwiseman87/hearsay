//! Opt-in sherpa-onnx offline diarization check on a real 2-speaker recording. Ignored by default
//! (needs the recording + the pyannote segmentation + wespeaker embedding ONNX models). Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference sherpa -- --ignored --nocapture

use std::collections::BTreeSet;
use std::path::PathBuf;

use hearsay_inference::{read_them_channel, SherpaDiarizer};

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

fn seg_model() -> PathBuf {
    repo("outputs/models/sherpa-onnx-pyannote-segmentation-3-0/model.onnx")
}

fn emb_model() -> PathBuf {
    repo("outputs/models/wespeaker_en_voxceleb_CAM++_LM.onnx")
}

/// Sweep the clustering threshold on the known 2-speaker clip to tune `DEFAULT_CLUSTER_THRESHOLD`.
#[test]
#[ignore = "tuning sweep; needs the recording + ONNX models"]
fn sweep_cluster_threshold() {
    let them = read_them_channel(repo(
        "outputs/recordings/2026-07-01_1833_miguel-kristina-test2/audio.wav",
    ))
    .expect("read Them channel");
    eprintln!("ground truth: 2 speakers");
    let embedders = [
        ("CAM++_LM", emb_model()),
        (
            "titanet_small",
            repo("outputs/models/nemo_en_titanet_small.onnx"),
        ),
    ];
    for (name, model) in &embedders {
        if !model.exists() {
            eprintln!("  [{name}] missing, skipping");
            continue;
        }
        for threshold in [0.5_f32, 0.6, 0.7, 0.8, 0.9] {
            let diarizer = SherpaDiarizer::load_with_threshold(&seg_model(), model, threshold)
                .expect("load diarizer");
            let result = diarizer.diarize(&them).expect("diarize");
            let speakers: BTreeSet<i64> = result.turns.iter().map(|t| t.speaker).collect();
            eprintln!(
                "  [{name}] threshold {threshold:.2} -> {} speakers, {} turns",
                speakers.len(),
                result.turns.len()
            );
        }
    }
}

#[test]
#[ignore = "needs a recording + pyannote segmentation + wespeaker embedding ONNX models"]
fn diarizes_real_two_speaker_meeting() {
    let audio = repo("outputs/recordings/2026-07-01_1833_miguel-kristina-test2/audio.wav");
    let them = read_them_channel(&audio).expect("read Them channel");
    let diarizer = SherpaDiarizer::load(
        &repo("outputs/models/sherpa-onnx-pyannote-segmentation-3-0/model.onnx"),
        &repo("outputs/models/wespeaker_en_voxceleb_CAM++_LM.onnx"),
    )
    .expect("load diarizer");

    let result = diarizer.diarize(&them).expect("diarize");
    let speakers: BTreeSet<i64> = result.turns.iter().map(|t| t.speaker).collect();
    let dim = result.embeddings.values().next().map(|v| v.len());
    eprintln!(
        "{} turns, {} speakers, {} voiceprints (dim {:?})",
        result.turns.len(),
        speakers.len(),
        result.embeddings.len(),
        dim
    );
    for turn in result.turns.iter().take(8) {
        eprintln!(
            "  Speaker {} [{:.1}-{:.1}]",
            turn.speaker, turn.start_s, turn.end_s
        );
    }

    assert!(!result.turns.is_empty(), "expected turns");
    assert!(
        speakers.len() >= 2,
        "expected >= 2 speakers, got {}",
        speakers.len()
    );
    // Each speaker carries a voiceprint (for cross-meeting recognition).
    assert_eq!(
        result.embeddings.len(),
        speakers.len(),
        "one voiceprint per speaker"
    );
}
