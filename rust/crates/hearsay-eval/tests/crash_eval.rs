//! Crash evaluation (`make crash-eval`), opt-in with `HEARSAY_CRASH_EVAL=1`: SIGKILLs the real
//! `hearsay-me` / `hearsay-live` sidecars mid-meeting and checks the live pipeline's respawn.
//!
//! The Me track is an AMI near-end speaker scored against its committed transcript; Them is another
//! AMI recording. Each scenario runs the full `Orchestrator` (paced `WavFileSource`, real
//! `ProcessTranscriber`s, in-memory DB) and is compared with a crash-free control run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hearsay_attribution::normalize;
use hearsay_db::models::{MeetingStatus, Stream as DbStream};
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;
use hearsay_engine::LiveEngine;
use hearsay_eval::echo::{
    densest_window, highpass, normalize_level, slice_utterances, TARGET_LEVEL,
};
use hearsay_eval::{
    load_utterances, resolve_audio, resolve_sidecar, write_report, Utterance, SAMPLE_RATE,
};
use hearsay_inference::read_wav_mono_16k;
use hearsay_orchestrator::{
    Backend, BackendInstance, LiveStats, LiveTuning, Orchestrator, ProcessTranscriber,
    WavFileSource,
};
use serde::Serialize;

const DEFAULT_WINDOW_S: f64 = 120.0;
const DEFAULT_SPEED: f64 = 2.0;
const FIRST_KILL_AUDIO_S: f64 = 20.0;
const SECOND_KILL_AFTER: Duration = Duration::from_secs(20);
const TAIL_SILENCE_S: usize = 3;
const READY_TIMEOUT: Duration = Duration::from_secs(300);
const PID_TIMEOUT: Duration = Duration::from_secs(60);
const END_SLACK: Duration = Duration::from_secs(8);
const MIC_HIGHPASS_HZ: f64 = 100.0;
const THEM_AUDIO: &str = "ami/ES2004a.Mix-Headset.wav";
const THEM_TRANSCRIPT: &str = "ES2004a.utterances.json";
const NEAR_AUDIO: &str = "ami/ES2004b.Headset-0.wav";
const NEAR_TRANSCRIPT: &str = "ES2004b-A.utterances.json";

const MIN_MATCH_SCORE: f64 = 0.5;
const MIN_MATCH_WORDS: usize = 3;
const MAX_MEDIAN_ERR_S: f64 = 3.0;
// The segment in flight when a sidecar dies is lost; live segments run up to about 25 s.
const MAX_LOST_S_PER_CRASH: f64 = 30.0;
const MIN_WORD_RATIO: f64 = 0.85;
const MIN_POST_CRASH_MATCHED: usize = 3;
const DUPLICATE_START_S: f64 = 1.0;

static PANICS: AtomicUsize = AtomicUsize::new(0);

struct Data {
    me: Vec<f32>,
    them: Vec<f32>,
    me_refs: Vec<Utterance>,
    window_s: f64,
}

fn enabled() -> bool {
    if std::env::var("HEARSAY_CRASH_EVAL").as_deref() != Ok("1") {
        eprintln!("crash-eval: opt-in (run `make crash-eval`); skipping");
        return false;
    }
    true
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

fn load_data() -> Option<Data> {
    let them_path = resolve_audio(THEM_AUDIO);
    let near_path = resolve_audio(NEAR_AUDIO);
    for path in [&them_path, &near_path] {
        if !path.exists() {
            eprintln!("crash-eval: audio absent ({}); skipping", path.display());
            return None;
        }
    }
    let them_full = read_wav_mono_16k(&them_path).expect("read Them audio");
    let near_full = read_wav_mono_16k(&near_path).expect("read near-end audio");
    let secs = |n: usize| n as f64 / SAMPLE_RATE as f64;
    let window_s = env_f64("HEARSAY_CRASH_WINDOW_S", DEFAULT_WINDOW_S)
        .min(secs(them_full.len()))
        .min(secs(near_full.len()));
    let them_utts = load_utterances(THEM_TRANSCRIPT);
    let near_utts = load_utterances(NEAR_TRANSCRIPT);
    let them_start = densest_window(&them_utts, window_s, secs(them_full.len()));
    let near_start = densest_window(&near_utts, window_s, secs(near_full.len()));
    let len = (window_s * SAMPLE_RATE as f64) as usize;
    let prepare = |full: &[f32], start_s: f64| {
        let start = ((start_s * SAMPLE_RATE as f64) as usize).min(full.len());
        let window = &full[start..(start + len).min(full.len())];
        normalize_level(&highpass(window, MIC_HIGHPASS_HZ), TARGET_LEVEL)
    };
    let me_refs = slice_utterances(&near_utts, near_start, near_start + window_s);
    eprintln!(
        "crash-eval: {window_s:.0}s window; Them from {them_start:.0}s, Me from {near_start:.0}s ({} reference utterances)",
        me_refs.len()
    );
    Some(Data {
        me: prepare(&near_full, near_start),
        them: prepare(&them_full, them_start),
        me_refs,
        window_s,
    })
}

fn write_stereo(path: &Path, data: &Data) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create stereo wav");
    let frames = data.me.len().max(data.them.len()) + TAIL_SILENCE_S * SAMPLE_RATE;
    for i in 0..frames {
        writer
            .write_sample(data.me.get(i).copied().unwrap_or(0.0))
            .unwrap();
        writer
            .write_sample(data.them.get(i).copied().unwrap_or(0.0))
            .unwrap();
    }
    writer.finalize().unwrap();
}

struct RunBackend {
    wav: PathBuf,
    speed: f64,
    me: Mutex<Option<ProcessTranscriber>>,
    them: Mutex<Option<ProcessTranscriber>>,
}

impl Backend for RunBackend {
    fn build(&self) -> BackendInstance {
        BackendInstance {
            source: Box::new(WavFileSource::new(self.wav.clone()).with_speed(self.speed)),
            me: Box::new(self.me.lock().unwrap().take().expect("one build per run")),
            them: Box::new(self.them.lock().unwrap().take().expect("one build per run")),
        }
    }
}

fn warm(binary: &Path) -> ProcessTranscriber {
    let mut transcriber = ProcessTranscriber::new(binary.to_path_buf());
    transcriber.spawn_warming().expect("spawn sidecar");
    transcriber
}

async fn wait_ready(me: &ProcessTranscriber, them: &ProcessTranscriber) {
    let deadline = Instant::now() + READY_TIMEOUT;
    while !(me.is_ready() && them.is_ready()) {
        assert!(Instant::now() < deadline, "sidecars never became ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Pids of this test process's own children named `name`.
fn child_pids(name: &str) -> Vec<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &std::process::id().to_string(), "-x", name])
        .output()
        .expect("run pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

async fn find_new_pid(name: &str, seen: &[u32]) -> u32 {
    let deadline = Instant::now() + PID_TIMEOUT;
    loop {
        if let Some(pid) = child_pids(name).into_iter().find(|p| !seen.contains(p)) {
            return pid;
        }
        assert!(Instant::now() < deadline, "no new {name} child appeared");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn sigkill(pid: u32) {
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -KILL {pid} failed");
}

#[derive(Clone, Copy)]
enum Trigger {
    AtAudio(f64),
    AfterPrevious(Duration),
    Immediate,
}

#[derive(Clone, Copy)]
struct Kill {
    sidecar: &'static str,
    trigger: Trigger,
}

struct Plan {
    name: &'static str,
    kills: Vec<Kill>,
}

fn plans() -> Vec<Plan> {
    let me = |trigger| Kill {
        sidecar: "hearsay-me",
        trigger,
    };
    let first = me(Trigger::AtAudio(FIRST_KILL_AUDIO_S));
    vec![
        Plan {
            name: "a-one-crash",
            kills: vec![first],
        },
        Plan {
            name: "b-two-crashes",
            kills: vec![first, me(Trigger::AfterPrevious(SECOND_KILL_AFTER))],
        },
        Plan {
            name: "c-them-crash",
            kills: vec![Kill {
                sidecar: "hearsay-live",
                trigger: Trigger::AtAudio(FIRST_KILL_AUDIO_S),
            }],
        },
        Plan {
            name: "d-kill-loop",
            kills: vec![
                first,
                me(Trigger::Immediate),
                me(Trigger::Immediate),
                me(Trigger::Immediate),
            ],
        },
    ]
}

#[derive(Clone)]
struct Final {
    start_s: f64,
    end_s: f64,
    words: Vec<String>,
}

struct Outcome {
    kill_audio_s: Vec<f64>,
    me: Vec<Final>,
    them_words: usize,
    me_respawns: u64,
    them_respawns: u64,
    dropped_chunks: u64,
    active_before_stop: bool,
    finalized: bool,
    panics: usize,
    wall_s: f64,
}

async fn run(sidecars: &Sidecars, data: &Data, speed: f64, kills: &[Kill]) -> Outcome {
    let started = Instant::now();
    let tmp = tempfile::tempdir().expect("temp dir");
    let wav = tmp.path().join("scenario.wav");
    write_stereo(&wav, data);
    let audio_s =
        data.me.len().max(data.them.len()) as f64 / SAMPLE_RATE as f64 + TAIL_SILENCE_S as f64;

    let me = warm(&sidecars.me);
    let them = warm(&sidecars.live);
    wait_ready(&me, &them).await;

    let stats = Arc::new(LiveStats::default());
    let tuning = LiveTuning {
        aec: None,
        echo_dedup: None,
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
    let panics_before = PANICS.load(Ordering::SeqCst);
    let meeting = orch.start_meeting(Some("crash-eval".into())).await.unwrap();
    let t0 = Instant::now();

    let mut seen: HashMap<&str, Vec<u32>> = HashMap::new();
    let mut kill_audio_s = Vec::new();
    let mut last_kill = t0;
    for kill in kills {
        match kill.trigger {
            Trigger::AtAudio(s) => {
                tokio::time::sleep_until((t0 + Duration::from_secs_f64(s / speed)).into()).await;
            }
            Trigger::AfterPrevious(after) => {
                tokio::time::sleep_until((last_kill + after).into()).await
            }
            Trigger::Immediate => {}
        }
        let known = seen.entry(kill.sidecar).or_default();
        let pid = find_new_pid(kill.sidecar, known).await;
        sigkill(pid);
        known.push(pid);
        last_kill = Instant::now();
        kill_audio_s.push(t0.elapsed().as_secs_f64() * speed);
        eprintln!(
            "crash-eval: killed {} pid {pid} at audio {:.1}s",
            kill.sidecar,
            kill_audio_s.last().unwrap()
        );
    }
    tokio::time::sleep_until((t0 + Duration::from_secs_f64(audio_s / speed) + END_SLACK).into())
        .await;
    let active_before_stop = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .is_some_and(|m| m.status == MeetingStatus::Recording);
    let stopped = orch.stop_meeting(meeting.id).await.unwrap().unwrap();
    orch.wait_for_refines().await;

    let segments = queries::list_segments(&pool, meeting.id).await.unwrap();
    let mut me: Vec<Final> = segments
        .iter()
        .filter(|s| s.stream == DbStream::Me)
        .map(|s| Final {
            start_s: s.start_s,
            end_s: s.end_s,
            words: normalize(&s.text),
        })
        .collect();
    me.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let them_words = segments
        .iter()
        .filter(|s| s.stream == DbStream::Them)
        .map(|s| normalize(&s.text).len())
        .sum();
    Outcome {
        kill_audio_s,
        me,
        them_words,
        me_respawns: stats.me_respawns.load(Ordering::SeqCst),
        them_respawns: stats.them_respawns.load(Ordering::SeqCst),
        dropped_chunks: stats.dropped_chunks.load(Ordering::SeqCst),
        active_before_stop,
        finalized: stopped.status == MeetingStatus::Finalized,
        panics: PANICS.load(Ordering::SeqCst) - panics_before,
        wall_s: started.elapsed().as_secs_f64(),
    }
}

struct Sidecars {
    me: PathBuf,
    live: PathBuf,
}

fn overlap_score(a: &[String], b: &[String]) -> f64 {
    let mut pool: HashMap<&str, usize> = HashMap::new();
    for w in b {
        *pool.entry(w.as_str()).or_default() += 1;
    }
    let shared = a
        .iter()
        .filter(|w| match pool.get_mut(w.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        })
        .count();
    shared as f64 / a.len().max(b.len()).max(1) as f64
}

/// The reference words in order, each with its time spread evenly across its utterance.
struct RefWords {
    words: Vec<String>,
    start_s: Vec<f64>,
    dur_s: Vec<f64>,
}

fn ref_words(utts: &[Utterance]) -> RefWords {
    let mut refs = RefWords {
        words: Vec::new(),
        start_s: Vec::new(),
        dur_s: Vec::new(),
    };
    for u in utts {
        let words = normalize(&u.text);
        let step = (u.end_s - u.start_s) / words.len().max(1) as f64;
        for (k, w) in words.into_iter().enumerate() {
            refs.words.push(w);
            refs.start_s.push(u.start_s + step * k as f64);
            refs.dur_s.push(step);
        }
    }
    refs
}

/// The reference span with the best word overlap with the final (ties go to the nearest in time),
/// as `(first reference word, start error in seconds)`.
fn align(f: &Final, refs: &RefWords) -> Option<(usize, f64)> {
    let len = f.words.len();
    if len < MIN_MATCH_WORDS || len > refs.words.len() {
        return None;
    }
    (0..=refs.words.len() - len)
        .map(|i| {
            (
                i,
                overlap_score(&f.words, &refs.words[i..i + len]),
                (f.start_s - refs.start_s[i]).abs(),
            )
        })
        .filter(|(_, score, _)| *score >= MIN_MATCH_SCORE)
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.2.total_cmp(&a.2)))
        .map(|(i, _, err)| (i, err))
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    })
}

/// Start errors (seconds) of the aligned finals starting at or after `from_s`, and how many finals
/// were considered.
fn errors_after(finals: &[Final], refs: &RefWords, from_s: f64) -> (Vec<f64>, usize) {
    let after: Vec<&Final> = finals.iter().filter(|f| f.start_s >= from_s).collect();
    let errors = after
        .iter()
        .filter_map(|f| align(f, refs).map(|(_, err)| err))
        .collect();
    (errors, after.len())
}

/// Reference words that no aligned Me final covers: count and seconds of speech.
fn lost_speech(finals: &[Final], refs: &RefWords) -> (usize, f64) {
    let mut covered = vec![false; refs.words.len()];
    for f in finals {
        if let Some((i, _)) = align(f, refs) {
            covered[i..i + f.words.len()].fill(true);
        }
    }
    let lost: Vec<usize> = (0..refs.words.len()).filter(|&i| !covered[i]).collect();
    (lost.len(), lost.iter().fold(0.0, |s, &i| s + refs.dur_s[i]))
}

fn duplicate_finals(finals: &[Final]) -> usize {
    finals
        .windows(2)
        .filter(|w| {
            w[0].words == w[1].words
                && w[0].words.len() >= MIN_MATCH_WORDS
                && (w[1].start_s - w[0].start_s).abs() < DUPLICATE_START_S
        })
        .count()
}

#[derive(Serialize)]
struct Row {
    scenario: String,
    kills_at_audio_s: Vec<f64>,
    me_respawns: u64,
    them_respawns: u64,
    me_finals: usize,
    me_words: usize,
    them_words: usize,
    post_crash_finals: usize,
    post_crash_matched: usize,
    median_err_s: Option<f64>,
    max_err_s: Option<f64>,
    control_median_err_s: Option<f64>,
    lost_words: usize,
    lost_s: f64,
    control_lost_s: f64,
    first_post_crash_final_lag_s: Option<f64>,
    last_final_end_s: f64,
    /// Every persisted Me final as (start s, end s, words).
    me_final_spans: Vec<(f64, f64, usize)>,
    duplicate_finals: usize,
    dropped_chunks: u64,
    meeting_active_before_stop: bool,
    meeting_finalized: bool,
    panics: usize,
    wall_s: f64,
    failures: Vec<String>,
}

fn fmt_opt(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_string(), |v| format!("{v:.2}"))
}

fn print_row(r: &Row) {
    eprintln!(
        "crash-eval: {:<14} respawn me/them {}/{} | me finals {:>3} words {:>4} them words {:>4} | post-crash {:>2}/{:>2} err median {:>5} max {:>5} (control {:>5}) | lost {:>3} words {:>5.1}s (control {:>5.1}s) lag {:>5} | dup {} chunks-lost {} | {} | {:>4.0}s",
        r.scenario,
        r.me_respawns,
        r.them_respawns,
        r.me_finals,
        r.me_words,
        r.them_words,
        r.post_crash_matched,
        r.post_crash_finals,
        fmt_opt(r.median_err_s),
        fmt_opt(r.max_err_s),
        fmt_opt(r.control_median_err_s),
        r.lost_words,
        r.lost_s,
        r.control_lost_s,
        fmt_opt(r.first_post_crash_final_lag_s),
        r.duplicate_finals,
        r.dropped_chunks,
        if r.failures.is_empty() { "ok" } else { "FAIL" },
        r.wall_s,
    );
    for failure in &r.failures {
        eprintln!("crash-eval:   FAILED {}: {failure}", r.scenario);
    }
}

fn build_row(name: &str, out: &Outcome, refs: &RefWords, control: &Outcome, window_s: f64) -> Row {
    let kills = out.kill_audio_s.len();
    let me_kill = name != "c-them-crash" && kills > 0;
    let first_kill = out.kill_audio_s.first().copied().unwrap_or(0.0);
    let last_kill = out.kill_audio_s.last().copied().unwrap_or(0.0);
    let from = if me_kill { first_kill } else { 0.0 };
    let (mut errors, post_finals) = errors_after(&out.me, refs, from);
    let (mut control_errors, _) = errors_after(&control.me, refs, from);
    let (lost_words, lost_s) = lost_speech(&out.me, refs);
    let (_, control_lost_s) = lost_speech(&control.me, refs);
    let me_words: usize = out.me.iter().map(|f| f.words.len()).sum();
    let control_me_words: usize = control.me.iter().map(|f| f.words.len()).sum();
    let matched = errors.len();
    let max_err = errors.iter().copied().max_by(f64::total_cmp);
    let median_err = median(&mut errors);
    let first_lag = me_kill
        .then(|| out.me.iter().find(|f| f.start_s >= first_kill))
        .flatten()
        .map(|f| f.start_s - first_kill);
    let last_final_end = out.me.iter().map(|f| f.end_s).fold(0.0, f64::max);

    let mut failures = Vec::new();
    let mut check = |ok: bool, msg: String| {
        if !ok {
            failures.push(msg);
        }
    };
    check(
        out.panics == 0,
        format!("{} panics during the run", out.panics),
    );
    check(
        out.active_before_stop,
        "meeting was not active before stop".to_string(),
    );
    check(
        out.finalized,
        "meeting did not finalize on stop".to_string(),
    );
    check(
        out.me.iter().all(|f| f.start_s >= -0.5)
            && last_final_end <= window_s + TAIL_SILENCE_S as f64 + 2.0,
        format!(
            "a final is out of the audio span: last end {last_final_end:.1}s of {window_s:.0}s"
        ),
    );
    check(
        duplicate_finals(&out.me) == 0,
        format!(
            "{} duplicate finals (audio fed twice?)",
            duplicate_finals(&out.me)
        ),
    );
    match name {
        "a-one-crash" | "b-two-crashes" => {
            check(
                out.me_respawns == kills as u64,
                format!("{} Me respawns for {kills} kills", out.me_respawns),
            );
            check(
                out.them_respawns == 0,
                format!("{} Them respawns", out.them_respawns),
            );
            check(
                matched >= MIN_POST_CRASH_MATCHED,
                format!("only {matched} post-crash finals matched a reference utterance ({post_finals} finals)"),
            );
            check(
                median_err.is_some_and(|m| m <= MAX_MEDIAN_ERR_S),
                format!(
                    "post-crash median start error {} s over {MAX_MEDIAN_ERR_S} s",
                    fmt_opt(median_err)
                ),
            );
            let (mut late, _) = errors_after(&out.me, refs, last_kill);
            check(
                median(&mut late).is_some_and(|m| m <= MAX_MEDIAN_ERR_S),
                format!(
                    "median start error after the last crash {} s over {MAX_MEDIAN_ERR_S} s",
                    fmt_opt(median(&mut late))
                ),
            );
            let allowed = control_lost_s + MAX_LOST_S_PER_CRASH * kills as f64;
            check(
                lost_s <= allowed,
                format!("{lost_s:.1}s of reference speech lost, over {allowed:.1}s allowed"),
            );
            check(
                out.them_words as f64 >= MIN_WORD_RATIO * control.them_words as f64,
                format!(
                    "Them words {} vs control {}",
                    out.them_words, control.them_words
                ),
            );
        }
        "c-them-crash" => {
            check(
                out.them_respawns == 1,
                format!("{} Them respawns", out.them_respawns),
            );
            check(
                out.me_respawns == 0,
                format!("{} Me respawns", out.me_respawns),
            );
            check(
                me_words as f64 >= MIN_WORD_RATIO * control_me_words as f64,
                format!("Me words {me_words} vs control {control_me_words}"),
            );
            check(
                median_err.is_some_and(|m| m <= MAX_MEDIAN_ERR_S),
                format!(
                    "Me median start error {} s over {MAX_MEDIAN_ERR_S} s",
                    fmt_opt(median_err)
                ),
            );
            check(
                out.them_words > 0,
                "no Them words after the Them crash".to_string(),
            );
        }
        "d-kill-loop" => {
            check(
                out.me_respawns == 3,
                format!(
                    "{} Me respawns, expected the 3-attempt budget",
                    out.me_respawns
                ),
            );
            check(
                out.me.iter().any(|f| f.start_s < first_kill),
                "no Me finals from before the first kill".to_string(),
            );
            check(
                out.me.iter().all(|f| f.start_s < last_kill),
                "Me finals appeared after the budget was exhausted".to_string(),
            );
            check(
                out.them_words as f64 >= MIN_WORD_RATIO * control.them_words as f64,
                format!(
                    "Them words {} vs control {}",
                    out.them_words, control.them_words
                ),
            );
            check(
                out.them_respawns == 0,
                format!("{} Them respawns", out.them_respawns),
            );
        }
        _ => {}
    }

    Row {
        scenario: name.to_string(),
        kills_at_audio_s: out.kill_audio_s.clone(),
        me_respawns: out.me_respawns,
        them_respawns: out.them_respawns,
        me_finals: out.me.len(),
        me_words,
        them_words: out.them_words,
        post_crash_finals: post_finals,
        post_crash_matched: matched,
        median_err_s: median_err,
        max_err_s: max_err,
        control_median_err_s: median(&mut control_errors),
        lost_words,
        lost_s,
        control_lost_s,
        first_post_crash_final_lag_s: first_lag,
        last_final_end_s: last_final_end,
        me_final_spans: out
            .me
            .iter()
            .map(|f| (f.start_s, f.end_s, f.words.len()))
            .collect(),
        duplicate_finals: duplicate_finals(&out.me),
        dropped_chunks: out.dropped_chunks,
        meeting_active_before_stop: out.active_before_stop,
        meeting_finalized: out.finalized,
        panics: out.panics,
        wall_s: out.wall_s,
        failures,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sidecar_crash_recovery() {
    if !enabled() {
        return;
    }
    let (Some(me), Some(live)) = (
        resolve_sidecar("HEARSAY_ME_BIN", "hearsay-me"),
        resolve_sidecar("HEARSAY_LIVE_BIN", "hearsay-live"),
    ) else {
        eprintln!("crash-eval: sidecars absent (make swift-build); skipping");
        return;
    };
    let Some(data) = load_data() else { return };
    let sidecars = Sidecars { me, live };
    let speed = env_f64("HEARSAY_CRASH_SPEED", DEFAULT_SPEED);
    let only = std::env::var("HEARSAY_CRASH_ONLY").ok();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::SeqCst);
        previous_hook(info);
    }));

    let refs = ref_words(&data.me_refs);
    for u in &data.me_refs {
        eprintln!(
            "crash-eval: reference {:>6.1}s - {:>6.1}s {:>3} words",
            u.start_s,
            u.end_s,
            normalize(&u.text).len()
        );
    }
    eprintln!("crash-eval: control run (no kills) at speed {speed}");
    let control = run(&sidecars, &data, speed, &[]).await;
    let mut rows: Vec<Row> = Vec::new();
    let control_row = build_row("control", &control, &refs, &control, data.window_s);
    print_row(&control_row);
    rows.push(control_row);

    for plan in plans() {
        if only.as_deref().is_some_and(|o| !plan.name.starts_with(o)) {
            continue;
        }
        eprintln!("crash-eval: scenario {}", plan.name);
        let out = run(&sidecars, &data, speed, &plan.kills).await;
        let row = build_row(plan.name, &out, &refs, &control, data.window_s);
        print_row(&row);
        rows.push(row);
    }
    let report = write_report("crash", &rows);
    eprintln!(
        "crash-eval: {} runs, window {:.0}s at speed {speed}; report -> {}",
        rows.len(),
        data.window_s,
        report.display()
    );
    let failed: Vec<String> = rows
        .iter()
        .flat_map(|r| r.failures.iter().map(|f| format!("{}: {f}", r.scenario)))
        .collect();
    assert!(
        failed.is_empty(),
        "crash eval failures:\n{}",
        failed.join("\n")
    );
}
