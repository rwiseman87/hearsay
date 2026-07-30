//! Diarization accuracy gate: run the macOS offline diarizer (`hearsay-diarize`) over a labeled local
//! corpus and fail if speaker-count error or DER regressed past the committed baseline. Passes on
//! equal-or-better, and **skips** (never fails) when the local audio / sidecar are absent — so it is a
//! plain `cargo test` safe inside `make ci` yet runs for real via `make diarize-eval` on a dev machine.
//!
//! It measures the **diarizer stage only** (the `Speaker N` turns FluidAudio emits), not the
//! whisper-transcribed refine: DER and speaker count are properties of the diarization turns, so
//! dropping whisper isolates exactly the signal Phase 2 tunes and removes whisper's cost +
//! nondeterminism. The whisper-dependent assembled-refine view stays in `refine_mac_probe`.
//!
//! Ground truth (committed): `diarization_corpus.json` names each reference by its speaker count and an
//! optional hand-labeled RTTM; `diarization_baseline.json` pins the current-config metrics. The audio
//! itself stays local (referenced by path, resolved under `HEARSAY_OUTPUT_DIR`). Re-baseline after an
//! intentional improvement with `HEARSAY_UPDATE_DIAR_BASELINE=1`.
//!
//! There is deliberately no Python scorer in the loop — DER is the pure-Rust `hearsay_attribution::der`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hearsay_attribution::{der, speaker_count, SpeakerTurn};
use hearsay_inference::{read_them_channel, read_wav_mono_16k, Diarizer, SwiftDiarizer};
use serde::{Deserialize, Serialize};

/// Forgiveness collar (s) around reference boundaries, matching the common AMI scoring convention.
const COLLAR_S: f64 = 0.25;
/// DER tolerance (absolute) absorbing CoreML run-to-run nondeterminism; count uses no slack.
const DER_EPSILON: f64 = 0.01;
/// Bound the diarize sidecar per reference, as the probe does.
const DIARIZE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
struct Corpus {
    references: Vec<Reference>,
}

#[derive(Deserialize)]
struct Reference {
    name: String,
    /// Absolute, or relative to the output dir (`HEARSAY_OUTPUT_DIR`, default repo `outputs/`).
    audio: String,
    #[serde(default)]
    channel: Channel,
    expected_speakers: usize,
    /// RTTM path relative to this crate's dir; enables DER on top of the count check.
    #[serde(default)]
    rttm: Option<String>,
}

#[derive(Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
enum Channel {
    /// Stereo Hearsay `audio.wav`: take the right (Them) channel.
    #[default]
    Them,
    /// Already a mono track (e.g. an AMI clip): use as-is.
    Mono,
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
    let diarize_bin = match resolve_diarize_bin() {
        Some(bin) => bin,
        None => {
            eprintln!(
                "diarize-eval: no hearsay-diarize sidecar (set HEARSAY_DIARIZE_BIN); skipping"
            );
            return;
        }
    };
    let output_dir = output_dir();
    let corpus: Corpus =
        serde_json::from_str(&fs::read_to_string(manifest_path()).expect("corpus"))
            .expect("parse diarization_corpus.json");

    let mut measured: Vec<Measured> = Vec::new();
    for reference in &corpus.references {
        let audio = resolve_audio(&reference.audio, &output_dir);
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

        let diarization = SwiftDiarizer::new(&diarize_bin, DIARIZE_TIMEOUT)
            .diarize(&them)
            .expect("diarize reference");
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
            let truth = parse_rttm(&crate_rel(rel));
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
    if std::env::var("HEARSAY_UPDATE_DIAR_BASELINE").as_deref() == Ok("1") {
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
            .expect("parse diarization_baseline.json");
    let failures = gate(&measured, &baseline);
    assert!(
        failures.is_empty(),
        "diarization accuracy regressed vs baseline:\n  {}",
        failures.join("\n  ")
    );
}

/// The pure non-regression decision: compare freshly-`measured` metrics against the committed
/// `baseline`, per reference and in aggregate, returning a human-readable failure per regression
/// (empty = passes). Equal-or-better passes; count uses no slack, DER a small `DER_EPSILON` for
/// CoreML nondeterminism. Separated from all I/O so it is unit-tested without the sidecar or audio.
fn gate(measured: &[Measured], baseline: &Baseline) -> Vec<String> {
    let current = build_baseline(measured);
    let mut failures: Vec<String> = Vec::new();
    for m in measured {
        match baseline.references.get(&m.name) {
            None => failures.push(format!(
                "{}: no baseline entry — run `HEARSAY_UPDATE_DIAR_BASELINE=1 make diarize-eval`",
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

/// Parse the SPEAKER lines of an RTTM into turns (columns: `SPEAKER file chan start dur NA NA spk ...`).
fn parse_rttm(path: &Path) -> Vec<SpeakerTurn> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("read rttm {}: {e}", path.display()));
    let mut turns = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() != Some(&"SPEAKER") || fields.len() < 8 {
            continue;
        }
        let (Ok(start), Ok(dur)) = (fields[3].parse::<f64>(), fields[4].parse::<f64>()) else {
            continue;
        };
        turns.push(SpeakerTurn {
            speaker: fields[7].to_string(),
            start_s: start,
            end_s: start + dur,
        });
    }
    turns
}

fn resolve_diarize_bin() -> Option<PathBuf> {
    if let Ok(bin) = std::env::var("HEARSAY_DIARIZE_BIN") {
        let path = PathBuf::from(bin);
        return path.exists().then_some(path);
    }
    [
        "helper/.build/debug/hearsay-diarize",
        "helper/.build/release/hearsay-diarize",
    ]
    .into_iter()
    .map(|rel| repo_root().join(rel))
    .find(|p| p.exists())
}

fn resolve_audio(audio: &str, output_dir: &Path) -> PathBuf {
    let path = PathBuf::from(audio);
    if path.is_absolute() {
        path
    } else {
        output_dir.join(path)
    }
}

fn output_dir() -> PathBuf {
    std::env::var("HEARSAY_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_root().join("outputs"))
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn crate_rel(rel: &str) -> PathBuf {
    crate_dir().join(rel)
}

/// The committed AMI corpus, or a local override (`HEARSAY_DIARIZE_CORPUS`) so a private recording can
/// be tuned against without committing it to the repo.
fn manifest_path() -> PathBuf {
    std::env::var("HEARSAY_DIARIZE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate_rel("tests/diarization_corpus.json"))
}

/// The committed baseline, or a local override (`HEARSAY_DIARIZE_BASELINE`) paired with a private
/// corpus so private metrics never touch the committed file.
fn baseline_path() -> PathBuf {
    std::env::var("HEARSAY_DIARIZE_BASELINE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate_rel("tests/diarization_baseline.json"))
}

fn repo_root() -> PathBuf {
    // crate dir = <repo>/rust/crates/hearsay-inference
    crate_dir()
        .ancestors()
        .nth(3)
        .expect("repo root above crate")
        .to_path_buf()
}

// Deterministic checks of the gate's decision logic — no audio, no sidecar, always run in CI.
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
