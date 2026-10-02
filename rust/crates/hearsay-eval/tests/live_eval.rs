//! Live accuracy and latency gate (`make live-eval`): feeds the corpus to `hearsay-me` and
//! `hearsay-live` concurrently at real-time pace, gating WER/cpWER and reporting final-delay latency.
//! Opt-in (`HEARSAY_LIVE_EVAL=1`); `HEARSAY_EVAL_SPEED=0` feeds unpaced, which skips latency, and
//! any speed other than 1 is report-only.

use std::collections::BTreeMap;
use std::path::Path;
use std::thread;

use hearsay_attribution::{cpwer, normalize, percentiles, word_errors, Percentiles};
use hearsay_eval::live::{final_delays_s, run_sidecar, LiveRun, LiveSegment};
use hearsay_eval::{
    eval_dir, gate, load_corpus, load_utterances, read_baseline, reference_streams, resolve_audio,
    resolve_sidecar, round4, update_baseline_requested, window, window_s, write_baseline,
    write_report, Baseline, Channel, GateOutcome, Metrics, SAMPLE_RATE,
};
use hearsay_inference::{read_them_channel, read_wav_mono_16k};
use serde::Serialize;

/// Live window default: dense enough speech to score, short enough for an on-demand run at real time.
const DEFAULT_LIVE_WINDOW_S: f64 = 600.0;
/// Live streaming is less repeatable than the offline pass, so it gets a wider tolerance.
const LIVE_EPSILON: f64 = 0.02;

#[derive(Serialize)]
struct SidecarReport {
    sidecar: &'static str,
    ready_s: f64,
    feed_wall_s: f64,
    exit_success: bool,
    finals: usize,
    partials: usize,
    reference_words: usize,
    substitutions: usize,
    deletions: usize,
    insertions: usize,
    wer: f64,
    cpwer: Option<f64>,
    speakers: usize,
    final_delay_s: Option<PercentilesReport>,
}

#[derive(Serialize)]
struct PercentilesReport {
    count: usize,
    p50: f64,
    p90: f64,
    max: f64,
}

impl From<Percentiles> for PercentilesReport {
    fn from(p: Percentiles) -> Self {
        Self {
            count: p.count,
            p50: p.p50,
            p90: p.p90,
            max: p.max,
        }
    }
}

#[derive(Serialize)]
struct LiveReport {
    name: String,
    window_s: f64,
    speed: f64,
    me: SidecarReport,
    them: SidecarReport,
}

fn speed() -> f64 {
    std::env::var("HEARSAY_EVAL_SPEED")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v >= 0.0)
        .unwrap_or(1.0)
}

fn score(
    sidecar: &'static str,
    run: &LiveRun,
    speed: f64,
    ref_by_speaker: &BTreeMap<String, Vec<String>>,
    ref_merged: &[String],
) -> SidecarReport {
    let mut finals: Vec<&LiveSegment> = run.segments.iter().filter(|s| s.kind == "final").collect();
    finals.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let hyp_merged: Vec<String> = finals.iter().flat_map(|s| normalize(&s.text)).collect();
    let mut hyp_by_speaker: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for segment in &finals {
        if let Some(slot) = segment.speaker {
            hyp_by_speaker
                .entry(slot.to_string())
                .or_default()
                .extend(normalize(&segment.text));
        }
    }
    let plain = word_errors(ref_merged, &hyp_merged);
    let attributed = if hyp_by_speaker.is_empty() {
        None
    } else {
        cpwer(ref_by_speaker, &hyp_by_speaker)
    };
    let delays = if speed > 0.0 {
        percentiles(&final_delays_s(&run.segments, speed))
    } else {
        None
    };
    SidecarReport {
        sidecar,
        ready_s: run.ready_s,
        feed_wall_s: run.feed_wall_s,
        exit_success: run.exit_success,
        finals: finals.len(),
        partials: run.segments.iter().filter(|s| s.kind == "partial").count(),
        reference_words: plain.reference_words,
        substitutions: plain.substitutions,
        deletions: plain.deletions,
        insertions: plain.insertions,
        wer: plain.wer(),
        cpwer: attributed.map(|c| c.breakdown.wer()),
        speakers: hyp_by_speaker.len(),
        final_delay_s: delays.map(PercentilesReport::from),
    }
}

fn describe(r: &SidecarReport) -> String {
    format!(
        "{}: WER {:.3} (S {} D {} I {} of {}) cpWER {} finals {} partials {} final delay p50/p90/max {} ready {:.0}s",
        r.sidecar,
        r.wer,
        r.substitutions,
        r.deletions,
        r.insertions,
        r.reference_words,
        r.cpwer.map_or_else(|| "n/a".to_string(), |v| format!("{v:.3}")),
        r.finals,
        r.partials,
        r.final_delay_s.as_ref().map_or_else(
            || "n/a".to_string(),
            |d| format!("{:.1}/{:.1}/{:.1}s", d.p50, d.p90, d.max)
        ),
        r.ready_s,
    )
}

#[test]
fn live_eval_gate() {
    // A real-time run takes as long as the window, so it never runs inside `cargo test` / `make ci`.
    if std::env::var("HEARSAY_LIVE_EVAL").as_deref() != Ok("1") {
        eprintln!("live-eval: opt-in (run `make live-eval`); skipping");
        return;
    }
    let Some(me_bin) = resolve_sidecar("HEARSAY_ME_BIN", "hearsay-me") else {
        eprintln!("live-eval: no hearsay-me sidecar (make swift-build); skipping");
        return;
    };
    let Some(live_bin) = resolve_sidecar("HEARSAY_LIVE_BIN", "hearsay-live") else {
        eprintln!("live-eval: no hearsay-live sidecar (make swift-build); skipping");
        return;
    };
    let window_s = window_s("HEARSAY_EVAL_LIVE_MAX_S", DEFAULT_LIVE_WINDOW_S);
    let speed = speed();
    let corpus = load_corpus();

    let mut reports: Vec<LiveReport> = Vec::new();
    for reference in &corpus.references {
        let audio_path = resolve_audio(&reference.audio);
        if !audio_path.exists() {
            eprintln!(
                "live-eval: skip {} (audio absent: {})",
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

        let (me_run, live_run) = run_concurrently(&me_bin, &live_bin, samples, speed);
        let me = score("hearsay-me", &me_run, speed, &ref_by_speaker, &ref_merged);
        let them = score(
            "hearsay-live",
            &live_run,
            speed,
            &ref_by_speaker,
            &ref_merged,
        );
        eprintln!(
            "live-eval: {} ({audio_s:.0}s at speed {speed})",
            reference.name
        );
        eprintln!("live-eval:   {}", describe(&me));
        eprintln!("live-eval:   {}", describe(&them));
        reports.push(LiveReport {
            name: reference.name.clone(),
            window_s,
            speed,
            me,
            them,
        });
    }

    if reports.is_empty() {
        eprintln!("live-eval: no reference audio present; nothing to gate");
        return;
    }
    let path = write_report("live", &reports);
    eprintln!("live-eval: report -> {}", path.display());
    if speed != 1.0 {
        eprintln!(
            "live-eval: speed {speed} is not the real-time baseline pace; report-only, no gate"
        );
        return;
    }

    let measured: BTreeMap<String, Metrics> = reports
        .iter()
        .map(|r| {
            let mut metrics = Metrics::new();
            metrics.insert("me_wer".to_string(), round4(r.me.wer));
            metrics.insert("them_wer".to_string(), round4(r.them.wer));
            if let Some(v) = r.them.cpwer {
                metrics.insert("them_cpwer".to_string(), round4(v));
            }
            (r.name.clone(), metrics)
        })
        .collect();
    let baseline_path = eval_dir().join("baseline-live.json");
    if update_baseline_requested() {
        write_baseline(
            &baseline_path,
            &Baseline {
                window_s,
                references: measured,
            },
        );
        eprintln!("live-eval: re-baselined -> {}", baseline_path.display());
        return;
    }
    let Some(baseline) = read_baseline(&baseline_path) else {
        panic!(
            "no baseline at {}; run with HEARSAY_UPDATE_EVAL_BASELINE=1",
            baseline_path.display()
        );
    };
    match gate(&measured, window_s, &baseline, LIVE_EPSILON) {
        GateOutcome::Pass => {}
        GateOutcome::Skipped(why) => eprintln!("live-eval: gate skipped ({why})"),
        GateOutcome::Failed(failures) => panic!(
            "live transcript accuracy regressed vs baseline:\n  {}",
            failures.join("\n  ")
        ),
    }
}

/// Run both sidecars at once on the same track, as they run in a meeting.
fn run_concurrently(
    me_bin: &Path,
    live_bin: &Path,
    samples: &[f32],
    speed: f64,
) -> (LiveRun, LiveRun) {
    thread::scope(|scope| {
        let me = scope.spawn(|| run_sidecar(me_bin, samples, speed));
        let live = scope.spawn(|| run_sidecar(live_bin, samples, speed));
        (
            me.join().expect("me thread").expect("run hearsay-me"),
            live.join().expect("live thread").expect("run hearsay-live"),
        )
    })
}
