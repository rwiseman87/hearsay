//! Shared plumbing for the accuracy and latency evals (`make wer-eval`, `make live-eval`): corpus and
//! reference-transcript loading, audio windowing, the non-regression gate against a committed
//! baseline, and run reports.
//!
//! Scoring itself is the pure `hearsay_attribution::{word_errors, cpwer, percentiles}`. The audio
//! stays local and is never committed; the manifest, the reference transcript and the baselines
//! under `shared/eval/` are. The diarization-only gate lives in `hearsay-inference`
//! (`make diarize-eval`) and is not duplicated here.

pub mod live;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hearsay_attribution::normalize;
use serde::{Deserialize, Serialize};

/// Sample rate every eval track is scored at.
pub const SAMPLE_RATE: usize = 16_000;

/// Default evaluation window in seconds (`HEARSAY_EVAL_MAX_S` overrides): long enough to cover the
/// whole AMI meeting. A shorter window keeps the dev
/// loop short; the baseline records the window it was measured on and only gates a matching one.
pub const DEFAULT_WINDOW_S: f64 = 1200.0;

/// Absolute tolerance on WER-style rates, absorbing run-to-run nondeterminism (CoreML and Metal).
pub const RATE_EPSILON: f64 = 0.01;

/// One reference utterance: a speaker's run of words with its time span in the meeting.
#[derive(Debug, Clone, Deserialize)]
pub struct Utterance {
    pub speaker: String,
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
}

/// The eval corpus manifest (`shared/eval/corpus.json`).
#[derive(Deserialize)]
pub struct Corpus {
    pub references: Vec<Reference>,
}

#[derive(Deserialize)]
pub struct Reference {
    pub name: String,
    /// Absolute, or relative to the output dir (`HEARSAY_OUTPUT_DIR`, default repo `outputs/`).
    pub audio: String,
    /// Mono track to use as-is, or the right (Them) channel of a stereo Hearsay `audio.wav`.
    #[serde(default)]
    pub channel: Channel,
    /// Reference transcript, relative to `shared/eval/`.
    pub transcript: String,
}

#[derive(Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Mono,
    Them,
}

/// The repository root. This crate sits at `rust/crates/hearsay-eval`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The committed eval data directory.
pub fn eval_dir() -> PathBuf {
    repo_root().join("shared/eval")
}

/// Where local audio and models live: `HEARSAY_OUTPUT_DIR`, else the repo `outputs/`.
pub fn output_dir() -> PathBuf {
    std::env::var_os("HEARSAY_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("outputs"))
}

/// A reference's audio path, resolved against the output dir when relative.
pub fn resolve_audio(audio: &str) -> PathBuf {
    let path = Path::new(audio);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        output_dir().join(path)
    }
}

/// A Swift sidecar binary: the path in `env_var` if set (and present), else the newest-looking build
/// under `helper/.build/`. `None` when absent, so a test can skip instead of failing.
pub fn resolve_sidecar(env_var: &str, name: &str) -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os(env_var) {
        let path = PathBuf::from(bin);
        return path.exists().then_some(path);
    }
    ["release", "debug"]
        .into_iter()
        .map(|profile| repo_root().join(format!("helper/.build/{profile}/{name}")))
        .find(|p| p.exists())
}

/// The whisper model the refine runs: `HEARSAY_REFINE_MODEL`, else the shipped default under the
/// output dir (`make fetch-refine-model`). `None` when absent.
pub fn resolve_refine_model() -> Option<PathBuf> {
    let path = std::env::var_os("HEARSAY_REFINE_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| output_dir().join("models/ggml-large-v3-turbo.bin"));
    path.exists().then_some(path)
}

pub fn load_corpus() -> Corpus {
    let path = std::env::var_os("HEARSAY_EVAL_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| eval_dir().join("corpus.json"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

pub fn load_utterances(transcript: &str) -> Vec<Utterance> {
    let path = eval_dir().join(transcript);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// The evaluation window in seconds: `HEARSAY_EVAL_MAX_S` (a positive number), else
/// [`DEFAULT_WINDOW_S`].
pub fn window_s() -> f64 {
    std::env::var("HEARSAY_EVAL_MAX_S")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(DEFAULT_WINDOW_S)
}

/// The leading `window_s` seconds of `samples`.
pub fn window(samples: &[f32], window_s: f64) -> &[f32] {
    let end = ((window_s * SAMPLE_RATE as f64) as usize).min(samples.len());
    &samples[..end]
}

/// Reference words inside the window: per speaker in time order, and all speakers merged in start
/// order. An utterance counts only when it ends inside the window, so a cut never splits one.
pub fn reference_streams(
    utterances: &[Utterance],
    window_s: f64,
) -> (BTreeMap<String, Vec<String>>, Vec<String>) {
    let mut inside: Vec<&Utterance> = utterances.iter().filter(|u| u.end_s <= window_s).collect();
    inside.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let mut per_speaker: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut merged: Vec<String> = Vec::new();
    for u in inside {
        let words = normalize(&u.text);
        merged.extend(words.iter().cloned());
        per_speaker
            .entry(u.speaker.clone())
            .or_default()
            .extend(words);
    }
    (per_speaker, merged)
}

/// Metric name to value. Every gated metric is lower-is-better.
pub type Metrics = BTreeMap<String, f64>;

/// The committed non-regression baseline: the window it was measured on and per-reference metrics.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Baseline {
    pub window_s: f64,
    #[serde(default)]
    pub references: BTreeMap<String, Metrics>,
}

#[derive(Debug, PartialEq)]
pub enum GateOutcome {
    Pass,
    /// The baseline was measured on a different window, so the comparison is not meaningful.
    Skipped(String),
    Failed(Vec<String>),
}

/// The pure non-regression decision: every `measured` metric must be at most the baseline value plus
/// `epsilon`. A missing baseline entry or metric fails, so a new reference or metric cannot slip in
/// ungated. Equal-or-better passes. Separated from all I/O so it is unit-tested without audio.
pub fn gate(
    measured: &BTreeMap<String, Metrics>,
    window_s: f64,
    baseline: &Baseline,
    epsilon: f64,
) -> GateOutcome {
    if (baseline.window_s - window_s).abs() > f64::EPSILON {
        return GateOutcome::Skipped(format!(
            "baseline was measured on a {}s window, this run used {window_s}s",
            baseline.window_s
        ));
    }
    let mut failures = Vec::new();
    for (name, metrics) in measured {
        let Some(base) = baseline.references.get(name) else {
            failures.push(format!("{name}: no baseline entry"));
            continue;
        };
        for (metric, value) in metrics {
            match base.get(metric) {
                None => failures.push(format!("{name}.{metric}: no baseline value")),
                Some(old) if *value > old + epsilon => failures.push(format!(
                    "{name}.{metric}: {value:.4} is worse than baseline {old:.4} (+{epsilon} allowed)"
                )),
                Some(_) => {}
            }
        }
    }
    if failures.is_empty() {
        GateOutcome::Pass
    } else {
        GateOutcome::Failed(failures)
    }
}

/// Write `baseline` to `path` as pretty JSON.
pub fn write_baseline(path: &Path, baseline: &Baseline) {
    fs::write(
        path,
        serde_json::to_string_pretty(baseline).expect("serialize baseline") + "\n",
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Read a baseline, or `None` when the file does not exist.
pub fn read_baseline(path: &Path) -> Option<Baseline> {
    let text = fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display())))
}

/// Round a metric for the committed baseline so a re-baseline does not churn the last digits.
pub fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Write a run report to `<output dir>/eval/<unix seconds>/<kind>.json` and return its path.
pub fn write_report(kind: &str, report: &impl Serialize) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let dir = output_dir().join("eval").join(stamp.to_string());
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let path = dir.join(format!("{kind}.json"));
    fs::write(
        &path,
        serde_json::to_string_pretty(report).expect("serialize report") + "\n",
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    path
}

/// Whether `HEARSAY_UPDATE_EVAL_BASELINE=1` asks for the baseline to be rewritten.
pub fn update_baseline_requested() -> bool {
    std::env::var("HEARSAY_UPDATE_EVAL_BASELINE").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(pairs: &[(&str, f64)]) -> Metrics {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    fn baseline(window_s: f64, wer: f64) -> Baseline {
        Baseline {
            window_s,
            references: BTreeMap::from([("ref".to_string(), metrics(&[("wer", wer)]))]),
        }
    }

    fn measured(wer: f64) -> BTreeMap<String, Metrics> {
        BTreeMap::from([("ref".to_string(), metrics(&[("wer", wer)]))])
    }

    #[test]
    fn equal_better_and_within_epsilon_pass() {
        let base = baseline(300.0, 0.20);
        assert_eq!(gate(&measured(0.20), 300.0, &base, 0.01), GateOutcome::Pass);
        assert_eq!(gate(&measured(0.15), 300.0, &base, 0.01), GateOutcome::Pass);
        assert_eq!(
            gate(&measured(0.205), 300.0, &base, 0.01),
            GateOutcome::Pass
        );
    }

    #[test]
    fn a_regression_beyond_epsilon_fails() {
        let base = baseline(300.0, 0.20);
        match gate(&measured(0.23), 300.0, &base, 0.01) {
            GateOutcome::Failed(failures) => assert!(failures[0].contains("ref.wer")),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn a_new_reference_or_metric_fails_until_baselined() {
        let base = Baseline {
            window_s: 300.0,
            references: BTreeMap::new(),
        };
        assert!(matches!(
            gate(&measured(0.1), 300.0, &base, 0.01),
            GateOutcome::Failed(_)
        ));
        let mut base = baseline(300.0, 0.2);
        base.references.get_mut("ref").unwrap().remove("wer");
        assert!(matches!(
            gate(&measured(0.1), 300.0, &base, 0.01),
            GateOutcome::Failed(_)
        ));
    }

    #[test]
    fn a_different_window_is_skipped_not_failed() {
        let base = baseline(300.0, 0.20);
        assert!(matches!(
            gate(&measured(0.9), 600.0, &base, 0.01),
            GateOutcome::Skipped(_)
        ));
    }

    #[test]
    fn window_truncates_to_whole_samples() {
        let samples = vec![0.0f32; SAMPLE_RATE * 3];
        assert_eq!(window(&samples, 2.0).len(), SAMPLE_RATE * 2);
        assert_eq!(window(&samples, 10.0).len(), SAMPLE_RATE * 3);
    }

    #[test]
    fn reference_streams_keep_only_utterances_that_end_inside_the_window() {
        let utterances = vec![
            Utterance {
                speaker: "B".into(),
                start_s: 5.0,
                end_s: 8.0,
                text: "Second, one.".into(),
            },
            Utterance {
                speaker: "A".into(),
                start_s: 0.0,
                end_s: 3.0,
                text: "First one".into(),
            },
            Utterance {
                speaker: "A".into(),
                start_s: 9.0,
                end_s: 12.0,
                text: "too late".into(),
            },
        ];
        let (per_speaker, merged) = reference_streams(&utterances, 10.0);
        assert_eq!(merged, vec!["first", "one", "second", "one"]);
        assert_eq!(per_speaker["A"], vec!["first", "one"]);
        assert_eq!(per_speaker["B"], vec!["second", "one"]);
        assert_eq!(per_speaker.len(), 2);
    }
}
