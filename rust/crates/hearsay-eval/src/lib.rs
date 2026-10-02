//! Shared plumbing for the accuracy and latency evals: corpus and transcript loading, windowing, the
//! baseline gate, and run reports. Scoring is the pure `hearsay_attribution` metrics; audio stays
//! local, while the manifest, transcript and baselines under `shared/eval/` are committed.

pub mod echo;
pub mod live;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hearsay_attribution::{normalize, SpeakerTurn};
use serde::{Deserialize, Serialize};

/// Sample rate every eval track is scored at.
pub const SAMPLE_RATE: usize = 16_000;

/// Default evaluation window in seconds (`HEARSAY_EVAL_MAX_S` overrides): the whole AMI meeting.
/// A baseline records its window and only gates a matching one.
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
    /// Absolute, or relative to the data dir (`HEARSAY_EVAL_DATA_DIR`, default repo `outputs/`).
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

/// Where local eval audio and run reports live: `HEARSAY_EVAL_DATA_DIR`, else the repo `outputs/`.
pub fn data_dir() -> PathBuf {
    std::env::var_os("HEARSAY_EVAL_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("outputs"))
}

/// A reference's audio path, resolved against the data dir when relative.
pub fn resolve_audio(audio: &str) -> PathBuf {
    let path = Path::new(audio);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        data_dir().join(path)
    }
}

/// Whether the opt-in variable `env_var` is `1`; prints why `label` skips otherwise.
pub fn opted_in(env_var: &str, label: &str, target: &str) -> bool {
    let on = std::env::var(env_var).as_deref() == Ok("1");
    if !on {
        eprintln!("{label}: opt-in with {env_var}=1 (run `make {target}`); skipping");
    }
    on
}

/// A Swift sidecar binary: the path in `env_var` if set (and present), else the debug, then the
/// release build under `helper/.build/`. `None` when absent, so a test can skip instead of failing.
pub fn resolve_sidecar(env_var: &str, name: &str) -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os(env_var) {
        let path = PathBuf::from(bin);
        return path.exists().then_some(path);
    }
    ["debug", "release"]
        .into_iter()
        .map(|profile| repo_root().join(format!("helper/.build/{profile}/{name}")))
        .find(|p| p.exists())
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

/// The evaluation window in seconds: `env_var` when it holds a positive number, else `default`.
pub fn window_s(env_var: &str, default: f64) -> f64 {
    std::env::var(env_var)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
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
/// ungated, and so does a baselined metric a measured reference no longer reports. Equal-or-better
/// passes. Separated from all I/O so it is unit-tested without audio.
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
        for metric in base.keys().filter(|m| !metrics.contains_key(*m)) {
            failures.push(format!("{name}.{metric}: not measured"));
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

/// Write a run report to `<data dir>/eval/<unix seconds>/<kind>.json` and return its path.
pub fn write_report(kind: &str, report: &impl Serialize) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let dir = data_dir().join("eval").join(stamp.to_string());
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

/// The SPEAKER turns of an RTTM file (columns: `SPEAKER file chan start dur NA NA spk ...`).
pub fn parse_rttm(path: &Path) -> Vec<SpeakerTurn> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("read rttm {}: {e}", path.display()));
    parse_rttm_text(&text)
}

fn parse_rttm_text(text: &str) -> Vec<SpeakerTurn> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.first() != Some(&"SPEAKER") || fields.len() < 8 {
                return None;
            }
            let start = fields[3].parse::<f64>().ok()?;
            let dur = fields[4].parse::<f64>().ok()?;
            Some(SpeakerTurn {
                speaker: fields[7].to_string(),
                start_s: start,
                end_s: start + dur,
            })
        })
        .collect()
}

/// The most times any block of up to 8 words repeats back to back (1 when nothing repeats) and that
/// block's length in words: the signature of an ASR repetition loop.
pub fn max_repeat_run(words: &[String]) -> (usize, usize) {
    let mut best = (usize::from(!words.is_empty()), 1);
    for period in 1..=8usize {
        for start in 0..words.len() {
            let unit = &words[start..(start + period).min(words.len())];
            if unit.len() < period {
                break;
            }
            let mut run = 1;
            while words.get(start + (run + 1) * period - 1).is_some()
                && words[start + run * period..start + (run + 1) * period] == *unit
            {
                run += 1;
            }
            if run > best.0 {
                best = (run, period);
            }
        }
    }
    best
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
    fn a_baselined_metric_that_was_not_measured_fails() {
        let mut base = baseline(300.0, 0.2);
        base.references
            .get_mut("ref")
            .unwrap()
            .insert("cpwer".to_string(), 0.3);
        assert_eq!(
            gate(&measured(0.2), 300.0, &base, 0.01),
            GateOutcome::Failed(vec!["ref.cpwer: not measured".to_string()])
        );
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

    fn w(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn counts_the_longest_back_to_back_repeat() {
        assert_eq!(max_repeat_run(&[]), (0, 1));
        assert_eq!(max_repeat_run(&w("a b c d")).0, 1);
        assert_eq!(max_repeat_run(&w("a a a b")), (3, 1));
        assert_eq!(max_repeat_run(&w("x a b a b a b y")), (3, 2));
        assert_eq!(max_repeat_run(&w("a b c a b c a b c a b c")), (4, 3));
    }

    #[test]
    fn separated_repeats_do_not_count_as_a_run() {
        assert_eq!(max_repeat_run(&w("a b x a b y a b")).0, 1);
    }

    #[test]
    fn parse_rttm_keeps_speaker_lines_with_numeric_times() {
        let text = "SPEAKER m 1 1.5 2.0 <NA> <NA> A <NA> <NA>\n\
                    ;; comment\n\
                    SPEAKER m 1 x 2.0 <NA> <NA> B <NA> <NA>\n\
                    SPEAKER m 1 4.0 0.5 <NA> <NA> B <NA> <NA>\n";
        let turns = parse_rttm_text(text);
        assert_eq!(turns.len(), 2);
        assert_eq!(
            (turns[0].speaker.as_str(), turns[0].start_s, turns[0].end_s),
            ("A", 1.5, 3.5)
        );
        assert_eq!(turns[1].speaker, "B");
    }
}
