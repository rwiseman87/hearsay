//! Echo evaluation (`make aec-eval`), report-only: how well the live Me stream copes with the Them
//! audio leaking into the mic. The remote track is a real AMI recording; the mic is synthesized from
//! a different AMI speaker plus a simulated room echo of that track.
//!
//! Stage A feeds the synthetic stereo straight through the Speex canceller and scores the audio.
//! Stage B runs the real pipeline (sidecars, canceller, echo dedup) and scores the persisted Me
//! words. Both need `--features aec` and opt in with `HEARSAY_ECHO_EVAL=1`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{
    env_f64, fmt_opt, load_tracks, wait_ready, warm, write_stereo, RunBackend, Sidecars,
    TAIL_SILENCE_S,
};
use hearsay_attribution::{normalize, word_errors};
use hearsay_db::models::Stream as DbStream;
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;
use hearsay_engine::LiveEngine;
use hearsay_eval::echo::{
    build_mix, convergence_s, echo_word_count, erle_db, fidelity, normalized_words,
    slice_utterances, EchoPath, Mix, Scenario,
};
use hearsay_eval::{data_dir, reference_streams, resolve_sidecar, write_report, SAMPLE_RATE};
use hearsay_orchestrator::{
    AecConfig, EchoCanceller, EchoDedupConfig, LiveStats, LiveTuning, Orchestrator,
};
use serde::Serialize;

const DEFAULT_WINDOW_S: f64 = 300.0;
const DEFAULT_SPEED: f64 = 4.0;
const DEFAULT_GRID: &str = "40:-8,40:-16,120:-8,120:-16";
const CONVERGED_ERLE_DB: f64 = 10.0;
/// The Speex preprocessor delays its output by one 160-sample frame.
const PREPROCESS_LATENCY: usize = 160;

struct Data {
    near: Vec<f32>,
    them: Vec<f32>,
    near_ref: Vec<String>,
    them_ref: Vec<String>,
    window_s: f64,
}

fn enabled() -> bool {
    if std::env::var("HEARSAY_ECHO_EVAL").as_deref() != Ok("1") {
        eprintln!("echo-eval: opt-in (run `make aec-eval`); skipping");
        return false;
    }
    if !cfg!(feature = "aec") {
        eprintln!("echo-eval: needs --features aec (use `make aec-eval`); skipping");
        return false;
    }
    true
}

fn grid() -> Vec<EchoPath> {
    let spec = std::env::var("HEARSAY_ECHO_GRID").unwrap_or_else(|_| DEFAULT_GRID.to_string());
    spec.split(',')
        .map(|point| {
            let (delay, level) = point
                .split_once(':')
                .unwrap_or_else(|| panic!("HEARSAY_ECHO_GRID point `{point}` is not delay:level"));
            EchoPath {
                delay_ms: delay.trim().parse().expect("delay ms"),
                level_db: level.trim().parse().expect("level dB"),
            }
        })
        .collect()
}

fn load_data() -> Option<Data> {
    let tracks = load_tracks(
        "echo-eval",
        env_f64("HEARSAY_ECHO_WINDOW_S", DEFAULT_WINDOW_S),
    )?;
    let window_s = tracks.window_s;
    let words_in = |utts, start: f64| {
        reference_streams(&slice_utterances(utts, start, start + window_s), window_s).1
    };
    let data = Data {
        near_ref: words_in(&tracks.near_utts, tracks.near_start),
        them_ref: words_in(&tracks.them_utts, tracks.them_start),
        near: tracks.near,
        them: tracks.them,
        window_s,
    };
    eprintln!(
        "echo-eval: {window_s:.0}s window; Them from {:.0}s ({} ref words), near-end from {:.0}s ({} ref words)",
        tracks.them_start,
        data.them_ref.len(),
        tracks.near_start,
        data.near_ref.len()
    );
    Some(data)
}

fn scenarios_for(path: EchoPath) -> [(Scenario, EchoPath); 2] {
    [(Scenario::EchoOnly, path), (Scenario::DoubleTalk, path)]
}

type AecVariant = (&'static str, Option<AecConfig>);

fn aec_variants() -> Vec<AecVariant> {
    let on = AecConfig::default();
    vec![
        ("off", None),
        ("pre-on", Some(on)),
        (
            "pre-off",
            Some(AecConfig {
                preprocess: false,
                ..on
            }),
        ),
    ]
}

fn fmt_convergence(value: Option<f64>) -> String {
    value.map_or_else(|| "never".to_string(), |v| format!("{v:.1}s"))
}

// ---------- Stage A: audio-only canceller metrics ----------

#[derive(Serialize)]
struct AudioRow {
    config: String,
    filter_length: usize,
    scenario: &'static str,
    delay_ms: f64,
    level_db: f64,
    erle_db: f64,
    convergence_s: Option<f64>,
    /// Near-end fidelity of the raw mic and of the canceller output (scenarios with near-end speech).
    mic_corr: Option<f64>,
    out_corr: Option<f64>,
    out_gain_db: Option<f64>,
}

fn cancel(mix: &Mix, config: Option<AecConfig>) -> Vec<f32> {
    let mut canceller = EchoCanceller::new_with(config);
    let mut out = vec![0.0f32; mix.mic.len()];
    let place = |out: &mut Vec<f32>, ready: Vec<(f64, Vec<f32>)>| {
        for (t0, samples) in ready {
            let start = (t0 * SAMPLE_RATE as f64).round() as usize;
            let end = (start + samples.len()).min(out.len());
            out[start..end].copy_from_slice(&samples[..end - start]);
        }
    };
    for (i, (me, them)) in mix.mic.chunks(1600).zip(mix.them.chunks(1600)).enumerate() {
        let t0 = (i * 1600) as f64 / SAMPLE_RATE as f64;
        place(&mut out, canceller.process_me(t0, me));
        place(&mut out, canceller.push_far(t0, them));
    }
    out
}

#[test]
fn aec_audio_metrics() {
    if !enabled() {
        return;
    }
    let Some(data) = load_data() else { return };
    let mut rows: Vec<AudioRow> = Vec::new();
    let mut variants = aec_variants();
    variants.retain(|(_, c)| c.is_some());
    let tails = [1600usize, AecConfig::default().filter_length];
    for (name, config) in variants {
        for tail in tails {
            let config = config.map(|c| AecConfig {
                filter_length: tail,
                ..c
            });
            let mut run = |scenario: Scenario, path: EchoPath| {
                let mix = build_mix(scenario, &data.near, &data.them, path);
                let out = cancel(&mix, config);
                let skip = 10 * SAMPLE_RATE;
                let echo = scenario != Scenario::NearEndOnly;
                let near = (scenario != Scenario::EchoOnly).then(|| {
                    (
                        fidelity(&mix.mic, &data.near, 0),
                        fidelity(&out, &data.near, PREPROCESS_LATENCY),
                    )
                });
                rows.push(AudioRow {
                    config: name.to_string(),
                    filter_length: tail,
                    scenario: scenario.name(),
                    delay_ms: path.delay_ms,
                    level_db: path.level_db,
                    erle_db: erle_db(&mix.mic, &out, skip),
                    convergence_s: echo
                        .then(|| convergence_s(&mix.mic, &out, CONVERGED_ERLE_DB))
                        .flatten(),
                    mic_corr: near.map(|(mic, _)| mic.envelope_corr),
                    out_corr: near.map(|(_, out)| out.envelope_corr),
                    out_gain_db: near.map(|(_, out)| out.band_gain_db),
                });
            };
            for path in grid() {
                for (scenario, path) in scenarios_for(path) {
                    run(scenario, path);
                }
            }
            run(
                Scenario::NearEndOnly,
                EchoPath {
                    delay_ms: 0.0,
                    level_db: -60.0,
                },
            );
        }
    }
    eprintln!(
        "echo-eval A: {:<8} {:>5} {:<13} {:>5} {:>5} | {:>8} {:>8} | {:>8} {:>8} {:>8}",
        "aec",
        "tail",
        "scenario",
        "delay",
        "level",
        "ERLE dB",
        "converge",
        "corr mic",
        "corr out",
        "gain dB"
    );
    for r in &rows {
        eprintln!(
            "echo-eval A: {:<8} {:>5} {:<13} {:>5.0} {:>5.0} | {:>8.1} {:>8} | {:>8} {:>8} {:>8}",
            r.config,
            r.filter_length,
            r.scenario,
            r.delay_ms,
            r.level_db,
            r.erle_db,
            if r.scenario == "near-end-only" {
                "-".to_string()
            } else {
                fmt_convergence(r.convergence_s)
            },
            fmt_opt(r.mic_corr),
            fmt_opt(r.out_corr),
            fmt_opt(r.out_gain_db),
        );
    }
    let path = write_report("echo-aec", &rows);
    eprintln!("echo-eval A: report -> {}", path.display());
}

// ---------- Stage B: the full live pipeline ----------

#[derive(Serialize, Default, Clone)]
struct WordScore {
    words: usize,
    insertions: usize,
    extra_words: usize,
    wer: Option<f64>,
    echo_words: usize,
}

#[derive(Serialize)]
struct PipelineRow {
    aec: String,
    vad_threshold: String,
    scenario: &'static str,
    delay_ms: f64,
    level_db: f64,
    dedup_on: WordScore,
    dedup_off: WordScore,
    echo_drops: usize,
    me_finals_on: usize,
    them_finals: usize,
    dropped_chunks: u64,
    wall_s: f64,
}

fn score(texts: &[String], reference: &[String], them_ref: &[String]) -> WordScore {
    let words: Vec<String> = texts.iter().flat_map(|t| normalize(t)).collect();
    let breakdown = word_errors(reference, &words);
    let matched = reference.len() - breakdown.substitutions - breakdown.deletions;
    let echo_words = echo_word_count(&normalized_words(texts), them_ref);
    WordScore {
        words: words.len(),
        insertions: breakdown.insertions,
        extra_words: words.len() - matched.min(words.len()),
        wer: (!reference.is_empty()).then(|| breakdown.wer()),
        echo_words,
    }
}

#[derive(Clone, Copy)]
struct Run {
    aec_name: &'static str,
    aec: Option<AecConfig>,
    vad: Option<f64>,
    scenario: Scenario,
    path: EchoPath,
}

async fn run_pipeline(sidecars: &Sidecars, data: &Data, speed: f64, run: &Run) -> PipelineRow {
    let Run {
        aec_name,
        aec,
        vad,
        scenario,
        path,
    } = *run;
    let started = Instant::now();
    let tmp = tempfile::tempdir().expect("temp dir");
    let wav = tmp.path().join("scenario.wav");
    let mix = build_mix(scenario, &data.near, &data.them, path);
    write_stereo(&wav, &mix.mic, &mix.them);
    let audio_s = (mix.mic.len() / SAMPLE_RATE + TAIL_SILENCE_S) as f64;

    let me = warm(&sidecars.me, vad);
    let them = warm(&sidecars.live, None);
    wait_ready(&me, &them).await;

    let stats = Arc::new(LiveStats::default());
    let tuning = LiveTuning {
        aec,
        echo_dedup: Some(EchoDedupConfig::default()),
        stats: Some(stats.clone()),
    };
    let pool = memory_pool().await;
    queries::set_preference(&pool, queries::SECTION_RECORDING, r#"{"record":false}"#)
        .await
        .expect("disable recording");
    let backend = Arc::new(RunBackend {
        wav,
        speed,
        me: Mutex::new(Some(me)),
        them: Mutex::new(Some(them)),
    });
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend)
        .with_tuning(tuning)
        .into_arc();
    let meeting = orch.start_meeting(Some("echo-eval".into())).await.unwrap();
    tokio::time::sleep(Duration::from_secs_f64(audio_s / speed + 2.0)).await;
    orch.stop_meeting(meeting.id).await.unwrap();
    orch.wait_for_refines().await;

    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let kept: Vec<(f64, String)> = segments
        .iter()
        .filter(|s| s.stream == DbStream::Me)
        .map(|s| (s.start_s, s.text.clone()))
        .collect();
    let mut all = kept.clone();
    all.extend(stats.echo_drops().into_iter().map(|d| (d.start_s, d.text)));
    let texts = |mut pairs: Vec<(f64, String)>| -> Vec<String> {
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        pairs.into_iter().map(|(_, t)| t).collect()
    };
    let reference: &[String] = match scenario {
        Scenario::EchoOnly => &[],
        _ => &data.near_ref,
    };
    PipelineRow {
        aec: aec_name.to_string(),
        vad_threshold: vad.map_or_else(|| "0.85".to_string(), |v| v.to_string()),
        scenario: scenario.name(),
        delay_ms: path.delay_ms,
        level_db: path.level_db,
        dedup_on: score(&texts(kept.clone()), reference, &data.them_ref),
        dedup_off: score(&texts(all), reference, &data.them_ref),
        echo_drops: stats.echo_drops().len(),
        me_finals_on: kept.len(),
        them_finals: segments
            .iter()
            .filter(|s| s.stream == DbStream::Them)
            .count(),
        dropped_chunks: stats
            .dropped_chunks
            .load(std::sync::atomic::Ordering::SeqCst),
        wall_s: started.elapsed().as_secs_f64(),
    }
}

fn print_row(r: &PipelineRow) {
    eprintln!(
        "echo-eval B: {:<8} {:>4} {:<13} {:>5.0} {:>5.0} | words {:>4}/{:>4} ins {:>4}/{:>4} echo {:>4}/{:>4} wer {:>5}/{:>5} | drops {:>3} chunks-lost {} {:>4.0}s",
        r.aec,
        r.vad_threshold,
        r.scenario,
        r.delay_ms,
        r.level_db,
        r.dedup_off.words,
        r.dedup_on.words,
        r.dedup_off.insertions,
        r.dedup_on.insertions,
        r.dedup_off.echo_words,
        r.dedup_on.echo_words,
        fmt_opt(r.dedup_off.wer),
        fmt_opt(r.dedup_on.wer),
        r.echo_drops,
        r.dropped_chunks,
        r.wall_s,
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_pipeline_echo() {
    if !enabled() {
        return;
    }
    let (Some(me), Some(live)) = (
        resolve_sidecar("HEARSAY_ME_BIN", "hearsay-me"),
        resolve_sidecar("HEARSAY_LIVE_BIN", "hearsay-live"),
    ) else {
        eprintln!("echo-eval: sidecars absent (make swift-build); skipping");
        return;
    };
    let Some(data) = load_data() else { return };
    let sidecars = Sidecars { me, live };
    let speed = env_f64("HEARSAY_ECHO_SPEED", DEFAULT_SPEED);
    let silent_path = EchoPath {
        delay_ms: 0.0,
        level_db: -60.0,
    };
    eprintln!(
        "echo-eval B: columns are dedup-off/dedup-on; aec | vad | scenario | delay ms | level dB"
    );

    let mut plan: Vec<Run> = Vec::new();
    for (aec_name, aec) in aec_variants() {
        for path in grid() {
            for (scenario, path) in scenarios_for(path) {
                plan.push(Run {
                    aec_name,
                    aec,
                    vad: None,
                    scenario,
                    path,
                });
            }
        }
        plan.push(Run {
            aec_name,
            aec,
            vad: None,
            scenario: Scenario::NearEndOnly,
            path: silent_path,
        });
    }
    for vad in [0.5, 0.7] {
        let run = |scenario, path| Run {
            aec_name: "pre-on",
            aec: Some(AecConfig::default()),
            vad: Some(vad),
            scenario,
            path,
        };
        for path in grid() {
            plan.push(run(Scenario::DoubleTalk, path));
        }
        plan.push(run(Scenario::NearEndOnly, silent_path));
    }

    let limit = env_f64("HEARSAY_ECHO_LIMIT", plan.len() as f64) as usize;
    let mut rows: Vec<PipelineRow> = Vec::new();
    for run in plan.iter().take(limit) {
        let row = run_pipeline(&sidecars, &data, speed, run).await;
        print_row(&row);
        rows.push(row);
    }
    let report = write_report("echo", &rows);
    eprintln!(
        "echo-eval B: {} runs, window {:.0}s at speed {speed}; report -> {}",
        rows.len(),
        data.window_s,
        report.display()
    );
    eprintln!("echo-eval B: outputs under {}", data_dir().display());
}
