//! Robustness check on local recordings (`make robustness-eval`): runs the real refine over each
//! `audio.wav` under `HEARSAY_ROBUSTNESS_DIR` and reports only counts (coverage, stalls, repeat runs,
//! RTF), never text, because these recordings have no reference transcript and stay private.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hearsay_attribution::normalize;
use hearsay_eval::{max_repeat_run, repo_root, resolve_sidecar, write_report, SAMPLE_RATE};
use hearsay_inference::{read_them_channel, refine_audio_file, InferenceError};
use serde::Serialize;

const REFINE_TIMEOUT: Duration = Duration::from_secs(1800);
/// A run this long of one repeated block is an ASR loop, not speech.
const LOOP_RUN: usize = 4;

#[derive(Serialize)]
struct Row {
    id: String,
    audio_s: f64,
    outcome: String,
    words: usize,
    words_per_audible_min: f64,
    speakers: usize,
    coverage: f64,
    audible_s: f64,
    stalls: usize,
    longest_stall_s: f64,
    longest_stall_start_s: f64,
    max_repeat_run: usize,
    repeat_unit_words: usize,
    loud_untranscribed_s: f64,
    refine_wall_s: f64,
    real_time_factor: f64,
}

#[test]
fn robustness_over_local_recordings() {
    let Some(dir) = std::env::var_os("HEARSAY_ROBUSTNESS_DIR").map(PathBuf::from) else {
        eprintln!("robustness: set HEARSAY_ROBUSTNESS_DIR (e.g. outputs/recordings); skipping");
        return;
    };
    let dir = if dir.is_absolute() {
        dir
    } else {
        repo_root().join(dir)
    };
    let Some(bin) = resolve_sidecar("HEARSAY_DIARIZE_BIN", "hearsay-diarize") else {
        eprintln!("robustness: no hearsay-diarize sidecar (make swift-build); skipping");
        return;
    };
    let mut audio: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path().join("audio.wav"))
                .collect()
        })
        .unwrap_or_default();
    audio.retain(|p| p.is_file());
    audio.sort();

    let mut rows = Vec::new();
    for (index, path) in audio.iter().enumerate() {
        let id = format!("rec-{:02}", index + 1);
        let samples = read_them_channel(path).expect("read recording");
        let audio_s = samples.len() as f64 / SAMPLE_RATE as f64;
        let started = Instant::now();
        let result = refine_audio_file(path, &bin, REFINE_TIMEOUT);
        let refine_wall_s = started.elapsed().as_secs_f64();
        let row = match result {
            Ok(output) => {
                let words: Vec<String> = output
                    .segments
                    .iter()
                    .flat_map(|s| normalize(&s.text))
                    .collect();
                let speakers: std::collections::BTreeSet<i64> =
                    output.segments.iter().map(|s| s.ordinal).collect();
                let coverage = output.coverage.expect("refine reports coverage");
                let audible_min = (coverage.audible_s / 60.0).max(f64::EPSILON);
                let (run, unit) = max_repeat_run(&words);
                let loud = loud_untranscribed_s(&samples, &output.segments);
                let longest = coverage
                    .uncovered
                    .iter()
                    .max_by(|a, b| (a.end - a.start).total_cmp(&(b.end - b.start)));
                Row {
                    id,
                    audio_s,
                    outcome: "ok".into(),
                    words: words.len(),
                    words_per_audible_min: words.len() as f64 / audible_min,
                    speakers: speakers.len(),
                    coverage: coverage.fraction(),
                    audible_s: coverage.audible_s,
                    stalls: coverage.uncovered.len(),
                    longest_stall_s: longest.map_or(0.0, |r| r.end - r.start),
                    longest_stall_start_s: longest.map_or(0.0, |r| r.start),
                    max_repeat_run: run,
                    repeat_unit_words: unit,
                    loud_untranscribed_s: loud,
                    refine_wall_s,
                    real_time_factor: refine_wall_s / audio_s.max(f64::EPSILON),
                }
            }
            Err(InferenceError::NoSpeech) => Row {
                id,
                audio_s,
                outcome: "no_speech".into(),
                words: 0,
                words_per_audible_min: 0.0,
                speakers: 0,
                coverage: 1.0,
                audible_s: 0.0,
                stalls: 0,
                longest_stall_s: 0.0,
                longest_stall_start_s: 0.0,
                max_repeat_run: 0,
                repeat_unit_words: 0,
                loud_untranscribed_s: 0.0,
                refine_wall_s,
                real_time_factor: refine_wall_s / audio_s.max(f64::EPSILON),
            },
            Err(e) => panic!("{id} refine failed: {e}"),
        };
        eprintln!(
            "robustness: {} {:.0}s {} words {} ({:.0}/min) spk {} coverage {:.2} stalls {} (max {:.0}s) repeat {}x{}w loud-untranscribed {:.0}s RTF {:.3}",
            row.id, row.audio_s, row.outcome, row.words, row.words_per_audible_min, row.speakers,
            row.coverage, row.stalls, row.longest_stall_s, row.max_repeat_run, row.repeat_unit_words, row.loud_untranscribed_s, row.real_time_factor
        );
        rows.push(row);
    }
    if rows.is_empty() {
        eprintln!("robustness: no recordings found under {}", dir.display());
        return;
    }
    let path = write_report("robustness", &rows);
    let loops = rows.iter().filter(|r| r.max_repeat_run >= LOOP_RUN).count();
    let low = rows
        .iter()
        .filter(|r| r.outcome == "ok" && r.coverage < 0.8)
        .count();
    eprintln!(
        "robustness: {} recordings, {loops} with a repeat run >= {LOOP_RUN}, {low} with coverage < 0.8; report -> {}",
        rows.len(),
        path.display()
    );
}

/// Seconds of one-second bins whose level is at least half the mean level of the transcribed bins but
/// that no segment covers: loud audio that went untranscribed, the signature of dropped speech.
fn loud_untranscribed_s(samples: &[f32], segments: &[hearsay_inference::RefinedSegment]) -> f64 {
    let rate = SAMPLE_RATE;
    let level = |bin: usize| -> f64 {
        let chunk =
            &samples[(bin * rate).min(samples.len())..((bin + 1) * rate).min(samples.len())];
        if chunk.is_empty() {
            return 0.0;
        }
        (chunk
            .iter()
            .map(|&s| f64::from(s) * f64::from(s))
            .sum::<f64>()
            / chunk.len() as f64)
            .sqrt()
    };
    let covered = |bin: usize| {
        let (start, end) = (bin as f64, bin as f64 + 1.0);
        segments.iter().any(|s| s.start_s < end && s.end_s > start)
    };
    let bins = samples.len().div_ceil(rate);
    let speech: Vec<f64> = (0..bins).filter(|&b| covered(b)).map(level).collect();
    if speech.is_empty() {
        return 0.0;
    }
    let floor = 0.5 * speech.iter().sum::<f64>() / speech.len() as f64;
    (0..bins)
        .filter(|&b| !covered(b) && level(b) >= floor)
        .count() as f64
}
