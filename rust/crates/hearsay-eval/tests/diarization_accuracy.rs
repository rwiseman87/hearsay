//! Diarization accuracy gate (`make diarize-eval`): runs the offline diarizer (`hearsay-diarize`
//! without `--asr`) over a labeled corpus; fails if speaker-count error or DER regressed past the
//! committed baseline. Opt-in (`HEARSAY_DIARIZE_EVAL=1`); self-skips without audio or sidecar.
//!
//! Committed: `shared/eval/diarization-corpus.json` (speaker count and optional RTTM per reference)
//! and `shared/eval/baseline-diarization.json`. Re-baseline with `HEARSAY_UPDATE_EVAL_BASELINE=1`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use hearsay_attribution::{der, speaker_count, SpeakerTurn};
use hearsay_eval::{
    eval_dir, opted_in, parse_rttm, resolve_audio, resolve_sidecar, update_baseline_requested,
    Channel,
};
use hearsay_inference::{diarize, read_them_channel, read_wav_mono_16k};
use serde::{Deserialize, Serialize};

/// Forgiveness collar (s) around reference boundaries, matching the common AMI scoring convention.
const COLLAR_S: f64 = 0.25;
/// DER tolerance (absolute) absorbing CoreML run-to-run nondeterminism; count uses no slack.
const DER_EPSILON: f64 = 0.01;
/// Bound the diarize sidecar per reference.
const DIARIZE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
struct Corpus {
    references: Vec<Reference>,
}

#[derive(Deserialize)]
struct Reference {
    name: String,
    /// Absolute, or relative to the data dir (`HEARSAY_EVAL_DATA_DIR`, default repo `outputs/`).
    audio: String,
    /// Defaults to the right (Them) channel of a stereo Hearsay `audio.wav`.
    #[serde(default = "them_channel")]
    channel: Channel,
    expected_speakers: usize,
    /// RTTM path relative to `shared/eval/`; enables DER on top of the count check.
    #[serde(default)]
    rttm: Option<String>,
}

fn them_channel() -> Channel {
    Channel::Them
}

#[derive(Serialize, Deserialize, Default)]
struct Baseline {
    #[serde(default)]
    references: BTreeMap<String, RefMetric>,
    #[serde(default)]
    aggregate: Option<AggregateMetric>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct RefMetric {
    count_abs_error: i64,
    #[serde(default)]
    der: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Default)]
struct AggregateMetric {
    mean_count_abs_error: f64,
    #[serde(default)]
    mean_der: Option<f64>,
}

struct Measured {
    name: String,
    signed_count_error: i64,
    count_abs_error: i64,
    der: Option<f64>,
}

#[test]
fn diarization_accuracy_gate() {
    if !opted_in("HEARSAY_DIARIZE_EVAL", "diarize-eval", "diarize-eval") {
        return;
    }
    let Some(diarize_bin) = resolve_sidecar("HEARSAY_DIARIZE_BIN", "hearsay-diarize") else {
        eprintln!("diarize-eval: no hearsay-diarize sidecar (make swift-build); skipping");
        return;
    };
    let corpus: Corpus =
        serde_json::from_str(&fs::read_to_string(manifest_path()).expect("corpus"))
            .expect("parse diarization corpus");

    let mut measured: Vec<Measured> = Vec::new();
    for reference in &corpus.references {
        let audio = resolve_audio(&reference.audio);
        if !audio.exists() {
            eprintln!(
                "diarize-eval: skip {} (audio absent: {})",
                reference.name,
                audio.display()
            );
            continue;
        }
        let them = match reference.channel {
            Channel::Them => read_them_channel(&audio),
            Channel::Mono => read_wav_mono_16k(&audio),
        }
        .expect("read reference audio");

        let diarization = diarize(&diarize_bin, &them, DIARIZE_TIMEOUT).expect("diarize reference");
        let hyp: Vec<SpeakerTurn> = diarization
            .turns
            .iter()
            .map(|t| SpeakerTurn {
                speaker: t.speaker.to_string(),
                start_s: t.start_s,
                end_s: t.end_s,
            })
            .collect();

        let got = speaker_count(&hyp);
        let signed = got as i64 - reference.expected_speakers as i64;
        let der_value = reference.rttm.as_ref().map(|rel| {
            let truth = parse_rttm(&eval_dir().join(rel));
            der(&truth, &hyp, COLLAR_S).der()
        });
        eprintln!(
            "diarize-eval: {} expected {} got {} (count err {:+}), DER {}",
            reference.name,
            reference.expected_speakers,
            got,
            signed,
            der_value.map_or_else(|| "n/a".to_string(), |d| format!("{d:.3}"))
        );
        measured.push(Measured {
            name: reference.name.clone(),
            signed_count_error: signed,
            count_abs_error: signed.abs(),
            der: der_value,
        });
    }

    if measured.is_empty() {
        eprintln!("diarize-eval: no reference audio present; nothing to gate");
        return;
    }

    let current = build_baseline(&measured);
    if update_baseline_requested() {
        fs::write(
            baseline_path(),
            serde_json::to_string_pretty(&current).expect("serialize baseline") + "\n",
        )
        .expect("write baseline");
        eprintln!(
            "diarize-eval: re-baselined {} reference(s) -> {}",
            measured.len(),
            baseline_path().display()
        );
        return;
    }

    let baseline: Baseline =
        serde_json::from_str(&fs::read_to_string(baseline_path()).expect("baseline"))
            .expect("parse diarization baseline");
    let failures = gate(&measured, &baseline);
    assert!(
        failures.is_empty(),
        "diarization accuracy regressed vs baseline:\n  {}",
        failures.join("\n  ")
    );
}

/// The pure non-regression decision, per reference and in aggregate (empty = passes). Count uses no
/// slack, DER allows `DER_EPSILON`; equal-or-better passes.
fn gate(measured: &[Measured], baseline: &Baseline) -> Vec<String> {
    let current = build_baseline(measured);
    let mut failures: Vec<String> = Vec::new();
    for m in measured {
        match baseline.references.get(&m.name) {
            None => failures.push(format!(
                "{}: no baseline entry — run `HEARSAY_UPDATE_EVAL_BASELINE=1 make diarize-eval`",
                m.name
            )),
            Some(base) => {
                if m.count_abs_error > base.count_abs_error {
                    failures.push(format!(
                        "{}: speaker-count error worsened |{:+}| ({} > baseline {})",
                        m.name, m.signed_count_error, m.count_abs_error, base.count_abs_error
                    ));
                }
                if let (Some(cur), Some(prev)) = (m.der, base.der) {
                    if cur > prev + DER_EPSILON {
                        failures.push(format!(
                            "{}: DER regressed {cur:.3} > baseline {prev:.3} (+{DER_EPSILON})",
                            m.name
                        ));
                    }
                }
            }
        }
    }
    if let (Some(cur), Some(prev)) = (current.aggregate, baseline.aggregate) {
        if cur.mean_count_abs_error > prev.mean_count_abs_error + f64::EPSILON {
            failures.push(format!(
                "aggregate mean count error worsened {:.3} > baseline {:.3}",
                cur.mean_count_abs_error, prev.mean_count_abs_error
            ));
        }
        if let (Some(c), Some(p)) = (cur.mean_der, prev.mean_der) {
            if c > p + DER_EPSILON {
                failures.push(format!(
                    "aggregate mean DER regressed {c:.3} > baseline {p:.3} (+{DER_EPSILON})"
                ));
            }
        }
    }
    failures
}

fn build_baseline(measured: &[Measured]) -> Baseline {
    let references: BTreeMap<String, RefMetric> = measured
        .iter()
        .map(|m| {
            (
                m.name.clone(),
                RefMetric {
                    count_abs_error: m.count_abs_error,
                    der: m.der,
                },
            )
        })
        .collect();
    let mean_count_abs_error = measured
        .iter()
        .map(|m| m.count_abs_error as f64)
        .sum::<f64>()
        / measured.len() as f64;
    let ders: Vec<f64> = measured.iter().filter_map(|m| m.der).collect();
    let mean_der = (!ders.is_empty()).then(|| ders.iter().sum::<f64>() / ders.len() as f64);
    Baseline {
        references,
        aggregate: Some(AggregateMetric {
            mean_count_abs_error,
            mean_der,
        }),
    }
}

/// The committed corpus, or a private one (`HEARSAY_DIARIZE_CORPUS`) kept out of the repo.
fn manifest_path() -> PathBuf {
    std::env::var_os("HEARSAY_DIARIZE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| eval_dir().join("diarization-corpus.json"))
}

/// The committed baseline, or a private one (`HEARSAY_DIARIZE_BASELINE`) for a private corpus.
fn baseline_path() -> PathBuf {
    std::env::var_os("HEARSAY_DIARIZE_BASELINE")
        .map(PathBuf::from)
        .unwrap_or_else(|| eval_dir().join("baseline-diarization.json"))
}

// Deterministic checks of the gate's decision logic: no audio, no sidecar.
mod gate_logic {
    use super::*;

    fn measured(name: &str, signed_count_error: i64, der: Option<f64>) -> Measured {
        Measured {
            name: name.to_string(),
            signed_count_error,
            count_abs_error: signed_count_error.abs(),
            der,
        }
    }

    #[test]
    fn maintaining_the_baseline_passes() {
        let current = [measured("m", 1, Some(0.20))];
        let baseline = build_baseline(&current);
        assert!(gate(&current, &baseline).is_empty());
    }

    #[test]
    fn improving_count_and_der_passes() {
        let baseline = build_baseline(&[measured("m", 2, Some(0.30))]);
        let better = [measured("m", 0, Some(0.10))];
        assert!(gate(&better, &baseline).is_empty());
    }

    #[test]
    fn worse_count_fails() {
        let baseline = build_baseline(&[measured("m", 0, None)]);
        // A worse count trips both the per-reference and the aggregate-mean checks.
        let failures = gate(&[measured("m", 1, None)], &baseline);
        assert!(failures.iter().any(|s| s.contains("speaker-count")));
        assert!(failures.iter().any(|s| s.contains("aggregate mean count")));
    }

    #[test]
    fn der_within_epsilon_passes_but_beyond_fails() {
        let baseline = build_baseline(&[measured("m", 0, Some(0.20))]);
        assert!(gate(&[measured("m", 0, Some(0.205))], &baseline).is_empty());
        let failures = gate(&[measured("m", 0, Some(0.25))], &baseline);
        assert!(failures.iter().any(|s| s.contains("DER regressed")));
    }

    #[test]
    fn missing_baseline_entry_fails() {
        let failures = gate(&[measured("new", 0, None)], &Baseline::default());
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("no baseline entry"));
    }
}
