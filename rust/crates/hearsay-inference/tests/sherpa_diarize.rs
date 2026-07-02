//! Opt-in sherpa-onnx offline diarization check on a real 2-speaker recording. Ignored by default
//! (needs the recording + the pyannote segmentation + wespeaker embedding ONNX models). Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference sherpa -- --ignored --nocapture

use std::collections::BTreeSet;
use std::path::PathBuf;

use hearsay_inference::{read_them_channel, DiarizeTuning, SherpaDiarizer};

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
    eprintln!("ground truth: 2 speakers (TitaNet embedder)");
    let titanet = repo("outputs/models/nemo_en_titanet_small.onnx");
    for min_on in [0.3_f32, 0.5, 1.0, 2.0] {
        for threshold in [0.80_f32, 0.90, 0.95, 0.97] {
            let tuning = DiarizeTuning {
                cluster_threshold: threshold,
                min_duration_on: min_on,
                min_duration_off: 0.5,
            };
            let diarizer =
                SherpaDiarizer::load_tuned(&seg_model(), &titanet, tuning).expect("load diarizer");
            let result = diarizer.diarize(&them).expect("diarize");
            let speakers: BTreeSet<i64> = result.turns.iter().map(|t| t.speaker).collect();
            let hit = if speakers.len() == 2 { "  <== 2" } else { "" };
            eprintln!(
                "  min_on {min_on:.1} threshold {threshold:.2} -> {} speakers, {} turns{hit}",
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
    let diarizer = SherpaDiarizer::load(&seg_model(), &emb_model()).expect("load diarizer");

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
