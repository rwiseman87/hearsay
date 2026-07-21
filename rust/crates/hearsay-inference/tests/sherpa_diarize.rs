#![cfg(feature = "sherpa")]
//! Opt-in sherpa-onnx offline diarization check on a real 2-speaker recording. Ignored by default
//! (needs the recording + the pyannote segmentation + TitaNet-small embedding ONNX models —
//! `make fetch-sherpa-models`). Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --features sherpa sherpa -- --ignored --nocapture

use std::collections::BTreeSet;
use std::path::PathBuf;

use hearsay_attribution::ConsolidateConfig;
use hearsay_inference::{read_them_channel, DiarizeTuning, Diarizer, SherpaDiarizer};

/// Consolidation disabled — nothing clears a cosine of 2.0 and nothing is ever judged non-speech —
/// so the sweep can report sherpa's raw cluster count next to the consolidated one.
const RAW: ConsolidateConfig = ConsolidateConfig {
    merge_threshold: 2.0,
    evidence_s: 0.0,
    non_voice_ceiling: f64::NEG_INFINITY,
};

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

fn seg_model() -> PathBuf {
    repo("outputs/models/sherpa/sherpa-onnx-pyannote-segmentation-3-0/model.onnx")
}

fn emb_model() -> PathBuf {
    repo("outputs/models/sherpa/nemo_en_titanet_small.onnx")
}

/// Sweep the clustering threshold on a known 2-speaker recording, reporting sherpa's raw cluster
/// count next to the count after [`consolidate_speakers`]. `HEARSAY_SWEEP_WAV` overrides the clip.
///
/// The operating point this documents (a 371 s 2-speaker clip): sherpa alone gives 28/14/10/6/5
/// clusters at thresholds 0.50/0.70/0.80/0.90/0.95, and consolidation brings every one of those
/// from 0.70 up to a clean 2 — so the result no longer hangs off the sherpa threshold at all.
#[test]
#[ignore = "tuning sweep; needs the recording + ONNX models"]
fn sweep_cluster_threshold() {
    let clip = std::env::var("HEARSAY_SWEEP_WAV").map_or_else(
        |_| repo("outputs/recordings/2026-07-01_1833_miguel-kristina-test2/audio.wav"),
        PathBuf::from,
    );
    let them = read_them_channel(clip).expect("read Them channel");
    eprintln!("ground truth: 2 speakers (TitaNet embedder)");
    let titanet = repo("outputs/models/sherpa/nemo_en_titanet_small.onnx");
    for min_on in [0.3_f32, 0.5, 1.0, 2.0] {
        for threshold in [0.80_f32, 0.90, 0.95, 0.97] {
            let mut counts = Vec::new();
            for consolidate in [RAW, ConsolidateConfig::default()] {
                let tuning = DiarizeTuning {
                    cluster_threshold: threshold,
                    min_duration_on: min_on,
                    min_duration_off: 0.5,
                    consolidate,
                };
                let diarizer = SherpaDiarizer::load_tuned(&seg_model(), &titanet, tuning)
                    .expect("load diarizer");
                let result = diarizer.diarize(&them).expect("diarize");
                let speakers: BTreeSet<i64> = result.turns.iter().map(|t| t.speaker).collect();
                counts.push(speakers.len());
            }
            let hit = if counts[1] == 2 { "  <== 2" } else { "" };
            eprintln!(
                "  min_on {min_on:.1} threshold {threshold:.2} -> {} raw -> {} consolidated{hit}",
                counts[0], counts[1]
            );
        }
    }
}

/// Raw-vs-consolidated speaker counts at the shipping tuning across a set of recordings
/// (`HEARSAY_DIAG_WAVS`, `;`-separated) — the over-merge regression check. Run it against a spread
/// of real meetings after touching the consolidation rules.
#[test]
#[ignore = "regression check; needs recordings + ONNX models"]
fn consolidates_across_recordings() {
    let wavs = std::env::var("HEARSAY_DIAG_WAVS").expect("set HEARSAY_DIAG_WAVS");
    for wav in wavs.split(';').filter(|w| !w.is_empty()) {
        let them = read_them_channel(PathBuf::from(wav)).expect("read Them channel");
        let mut counts = Vec::new();
        for consolidate in [RAW, ConsolidateConfig::default()] {
            let tuning = DiarizeTuning {
                consolidate,
                ..Default::default()
            };
            let diarizer =
                SherpaDiarizer::load_tuned(&seg_model(), &emb_model(), tuning).expect("load");
            let result = diarizer.diarize(&them).expect("diarize");
            let speakers: BTreeSet<i64> = result.turns.iter().map(|t| t.speaker).collect();
            counts.push(speakers.len());
        }
        eprintln!(
            "  {:>5.0}s  {} raw -> {} consolidated  {}",
            them.len() as f64 / 16000.0,
            counts[0],
            counts[1],
            PathBuf::from(wav)
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
    }
}

#[test]
#[ignore = "needs a recording + pyannote segmentation + TitaNet-small embedding ONNX models"]
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
