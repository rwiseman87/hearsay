//! Offline transcript accuracy gate (`make wer-eval`): runs the real refine over the labeled corpus,
//! scores WER and cpWER, and fails on a regression past `shared/eval/baseline-asr.json`.
//! Opt-in (`HEARSAY_WER_EVAL=1`); `HEARSAY_EVAL_ASR=<model>` scores another Parakeet model
//! report-only. Self-skips without audio or sidecar.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use hearsay_attribution::{cpwer, normalize, word_errors};
use hearsay_eval::{
    eval_dir, gate, load_corpus, load_utterances, opted_in, read_baseline, reference_streams,
    resolve_audio, resolve_sidecar, round4, update_baseline_requested, window, window_s,
    write_baseline, write_report, Baseline, Channel, GateOutcome, Metrics, DEFAULT_WINDOW_S,
    RATE_EPSILON, SAMPLE_RATE,
};
use hearsay_inference::{read_them_channel, read_wav_mono_16k, refine_them, ASR_MODEL};
use serde::Serialize;

const REFINE_TIMEOUT: Duration = Duration::from_secs(1800);

#[derive(Serialize)]
struct AsrReport {
    name: String,
    model: String,
    window_s: f64,
    audio_s: f64,
    refine_wall_s: f64,
    real_time_factor: f64,
    reference_words: usize,
    substitutions: usize,
    deletions: usize,
    insertions: usize,
    wer: f64,
    cpwer: Option<f64>,
    reference_speakers: usize,
    hypothesis_speakers: usize,
    coverage: Option<f64>,
}

#[test]
fn asr_accuracy_gate() {
    if !opted_in("HEARSAY_WER_EVAL", "wer-eval", "wer-eval") {
        return;
    }
    let Some(diarize_bin) = resolve_sidecar("HEARSAY_DIARIZE_BIN", "hearsay-diarize") else {
        eprintln!("wer-eval: no hearsay-diarize sidecar (make swift-build); skipping");
        return;
    };
    let model = std::env::var("HEARSAY_EVAL_ASR")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| ASR_MODEL.to_string());
    let window_s = window_s("HEARSAY_EVAL_MAX_S", DEFAULT_WINDOW_S);
    let corpus = load_corpus();

    let mut reports: Vec<AsrReport> = Vec::new();
    for reference in &corpus.references {
        let audio_path = resolve_audio(&reference.audio);
        if !audio_path.exists() {
            eprintln!(
                "wer-eval: skip {} (audio absent: {})",
                reference.name,
                audio_path.display()
            );
            continue;
        }
        let full = match reference.channel {
            Channel::Mono => read_wav_mono_16k(&audio_path),
            Channel::Them => read_them_channel(&audio_path),
        }
        .expect("read reference audio");
        let samples = window(&full, window_s);
        let audio_s = samples.len() as f64 / SAMPLE_RATE as f64;

        let utterances = load_utterances(&reference.transcript);
        let (ref_by_speaker, ref_merged) = reference_streams(&utterances, window_s.min(audio_s));

        let started = Instant::now();
        let output =
            refine_them(&diarize_bin, &model, samples, REFINE_TIMEOUT).expect("refine reference");
        let refine_wall_s = started.elapsed().as_secs_f64();

        let mut segments = output.segments.clone();
        segments.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        let hyp_merged: Vec<String> = segments.iter().flat_map(|s| normalize(&s.text)).collect();
        let mut hyp_by_speaker: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for segment in &segments {
            hyp_by_speaker
                .entry(segment.ordinal.to_string())
                .or_default()
                .extend(normalize(&segment.text));
        }

        let plain = word_errors(&ref_merged, &hyp_merged);
        let attributed = cpwer(&ref_by_speaker, &hyp_by_speaker);
        let report = AsrReport {
            name: reference.name.clone(),
            model: model.clone(),
            window_s,
            audio_s,
            refine_wall_s,
            real_time_factor: refine_wall_s / audio_s,
            reference_words: plain.reference_words,
            substitutions: plain.substitutions,
            deletions: plain.deletions,
            insertions: plain.insertions,
            wer: plain.wer(),
            cpwer: attributed.as_ref().map(|c| c.breakdown.wer()),
            reference_speakers: ref_by_speaker.len(),
            hypothesis_speakers: hyp_by_speaker.len(),
            coverage: output.coverage.as_ref().map(|c| c.fraction()),
        };
        eprintln!(
            "wer-eval: {} [{}] WER {:.3} (S {} D {} I {} of {}) cpWER {} speakers {}/{} coverage {} RTF {:.3} ({:.0}s audio in {:.0}s)",
            report.name,
            report.model,
            report.wer,
            report.substitutions,
            report.deletions,
            report.insertions,
            report.reference_words,
            report
                .cpwer
                .map_or_else(|| "n/a".to_string(), |v| format!("{v:.3}")),
            report.hypothesis_speakers,
            report.reference_speakers,
            report
                .coverage
                .map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}")),
            report.real_time_factor,
            report.audio_s,
            report.refine_wall_s,
        );
        reports.push(report);
    }

    if reports.is_empty() {
        eprintln!("wer-eval: no reference audio present; nothing to gate");
        return;
    }
    let path = write_report("asr", &reports);
    eprintln!("wer-eval: report -> {}", path.display());

    if model != ASR_MODEL {
        eprintln!("wer-eval: {model} is not the shipped model ({ASR_MODEL}); report-only, no gate");
        return;
    }
    let measured: BTreeMap<String, Metrics> = reports
        .iter()
        .map(|r| {
            let mut metrics = Metrics::new();
            metrics.insert("wer".to_string(), round4(r.wer));
            if let Some(v) = r.cpwer {
                metrics.insert("cpwer".to_string(), round4(v));
            }
            (r.name.clone(), metrics)
        })
        .collect();
    let baseline_path = eval_dir().join("baseline-asr.json");
    if update_baseline_requested() {
        write_baseline(
            &baseline_path,
            &Baseline {
                window_s,
                references: measured,
            },
        );
        eprintln!("wer-eval: re-baselined -> {}", baseline_path.display());
        return;
    }
    let Some(baseline) = read_baseline(&baseline_path) else {
        panic!(
            "no baseline at {}; run with HEARSAY_UPDATE_EVAL_BASELINE=1",
            baseline_path.display()
        );
    };
    match gate(&measured, window_s, &baseline, RATE_EPSILON) {
        GateOutcome::Pass => {}
        GateOutcome::Skipped(why) => eprintln!("wer-eval: gate skipped ({why})"),
        GateOutcome::Failed(failures) => panic!(
            "transcript accuracy regressed vs baseline:\n  {}",
            failures.join("\n  ")
        ),
    }
}
