//! Offline transcript accuracy gate (`make wer-eval`): run the real refine (whisper + the
//! `hearsay-diarize` sidecar) over the labeled local corpus and score the assembled transcript with
//! WER and cpWER against the committed reference, then fail if either regressed past the baseline.
//!
//! Self-skips (never fails) when the audio, whisper model, or sidecar is absent, so it is safe inside
//! `cargo test` and `make ci`. Latency-style numbers (refine wall time, RTF) go in the run report
//! only; they depend on the machine. Re-baseline after an intentional change with
//! `HEARSAY_UPDATE_EVAL_BASELINE=1`.
//!
//! `HEARSAY_EVAL_ASR=parakeet:<v2|v3|ultra|redux|phonon2>` scores the sidecar's Parakeet batch ASR
//! (words attributed to its diarizer turns) on the same metrics instead of the whisper refine. Those
//! runs are report-only: they are never gated and never written to the baseline.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hearsay_attribution::{cpwer, normalize, word_errors};
use hearsay_eval::{
    eval_dir, gate, load_corpus, load_utterances, parakeet, read_baseline, reference_streams,
    resolve_audio, resolve_refine_model, resolve_sidecar, round4, update_baseline_requested,
    window, window_s, write_baseline, write_report, Baseline, Channel, GateOutcome, Metrics,
    RATE_EPSILON, SAMPLE_RATE,
};
use hearsay_inference::{
    read_them_channel, read_wav_mono_16k, refine_them_with, SwiftDiarizer, WhisperAsr,
};
use serde::Serialize;

const DIARIZE_TIMEOUT: Duration = Duration::from_secs(1800);

enum Backend {
    Whisper { model: PathBuf, carry_over: bool },
    Parakeet { model: String },
}

impl Backend {
    fn label(&self) -> String {
        match self {
            Backend::Whisper { model, carry_over } => {
                format!("whisper:{}:carry_over={carry_over}", model.display())
            }
            Backend::Parakeet { model } => format!("parakeet:{model}"),
        }
    }
}

#[derive(Serialize)]
struct AsrReport {
    name: String,
    backend: String,
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

/// What a backend produced for one track.
struct Hypothesis {
    merged: Vec<String>,
    by_speaker: BTreeMap<String, Vec<String>>,
    wall_s: f64,
    coverage: Option<f64>,
}

fn resolve_backend() -> Option<Backend> {
    match std::env::var("HEARSAY_EVAL_ASR").ok().as_deref() {
        None | Some("") | Some("whisper") => {
            let Some(model) = resolve_refine_model() else {
                eprintln!("wer-eval: no whisper model (make fetch-refine-model); skipping");
                return None;
            };
            Some(Backend::Whisper {
                model,
                carry_over: std::env::var("HEARSAY_EVAL_CARRY_OVER").as_deref() == Ok("1"),
            })
        }
        Some(other) => match other.strip_prefix("parakeet:") {
            Some(model) if !model.is_empty() => Some(Backend::Parakeet {
                model: model.to_string(),
            }),
            _ => panic!("HEARSAY_EVAL_ASR must be `whisper` or `parakeet:<model>`, got {other:?}"),
        },
    }
}

fn transcribe_whisper(
    asr: &WhisperAsr,
    diarize_bin: &std::path::Path,
    samples: &[f32],
) -> Hypothesis {
    let diarizer = SwiftDiarizer::new(diarize_bin, DIARIZE_TIMEOUT);
    let started = Instant::now();
    let output = refine_them_with(asr, &diarizer, samples).expect("refine reference");
    let wall_s = started.elapsed().as_secs_f64();
    let mut segments = output.segments.clone();
    segments.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let merged = segments.iter().flat_map(|s| normalize(&s.text)).collect();
    let mut by_speaker: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for segment in &segments {
        by_speaker
            .entry(segment.ordinal.to_string())
            .or_default()
            .extend(normalize(&segment.text));
    }
    Hypothesis {
        merged,
        by_speaker,
        wall_s,
        coverage: output.coverage.as_ref().map(|c| c.fraction()),
    }
}

fn transcribe_parakeet(diarize_bin: &std::path::Path, model: &str, samples: &[f32]) -> Hypothesis {
    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("window.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).expect("create window wav");
    for sample in samples {
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * 32767.0) as i16)
            .expect("write sample");
    }
    writer.finalize().expect("finalize window wav");

    let started = Instant::now();
    let output = parakeet::run(diarize_bin, &wav, model).expect("run parakeet");
    let wall_s = started.elapsed().as_secs_f64();
    let asr = output.asr.expect("sidecar returned no asr payload");
    eprintln!(
        "wer-eval: parakeet:{model} decode {:.1}s of the {:.1}s sidecar run",
        asr.processing_s, wall_s
    );
    let (merged, by_speaker) = parakeet::attribute(&asr.words, &output.turns);
    Hypothesis {
        merged,
        by_speaker,
        wall_s,
        coverage: None,
    }
}

#[test]
fn asr_accuracy_gate() {
    let Some(backend) = resolve_backend() else {
        return;
    };
    let Some(diarize_bin) = resolve_sidecar("HEARSAY_DIARIZE_BIN", "hearsay-diarize") else {
        eprintln!("wer-eval: no hearsay-diarize sidecar (make swift-build); skipping");
        return;
    };
    let window_s = window_s();
    let corpus = load_corpus();
    let whisper = match &backend {
        Backend::Whisper { model, carry_over } => Some(
            WhisperAsr::load(model)
                .expect("load whisper model")
                .with_carry_over(*carry_over),
        ),
        Backend::Parakeet { .. } => None,
    };

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

        let hypothesis = match (&backend, &whisper) {
            (Backend::Whisper { .. }, Some(asr)) => transcribe_whisper(asr, &diarize_bin, samples),
            (Backend::Parakeet { model }, _) => transcribe_parakeet(&diarize_bin, model, samples),
            (Backend::Whisper { .. }, None) => unreachable!("whisper backend loads its model"),
        };

        let plain = word_errors(&ref_merged, &hypothesis.merged);
        let attributed = cpwer(&ref_by_speaker, &hypothesis.by_speaker);
        let report = AsrReport {
            name: reference.name.clone(),
            backend: backend.label(),
            window_s,
            audio_s,
            refine_wall_s: hypothesis.wall_s,
            real_time_factor: hypothesis.wall_s / audio_s,
            reference_words: plain.reference_words,
            substitutions: plain.substitutions,
            deletions: plain.deletions,
            insertions: plain.insertions,
            wer: plain.wer(),
            cpwer: attributed.as_ref().map(|c| c.breakdown.wer()),
            reference_speakers: ref_by_speaker.len(),
            hypothesis_speakers: hypothesis.by_speaker.len(),
            coverage: hypothesis.coverage,
        };
        eprintln!(
            "wer-eval: {} [{}] WER {:.3} (S {} D {} I {} of {}) cpWER {} speakers {}/{} RTF {:.3} ({:.0}s audio in {:.0}s)",
            report.name,
            report.backend,
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

    if !matches!(backend, Backend::Whisper { .. }) {
        eprintln!("wer-eval: non-whisper backend is report-only; no gate");
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
