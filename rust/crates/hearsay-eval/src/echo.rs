//! Synthetic echo scenarios and their scoring for `make aec-eval`: pure, deterministic signal math
//! with no I/O, so it is unit-tested without audio or sidecars.
//!
//! The mic is `near_end + echo(Them) + noise`, where the echo is Them through a short synthetic
//! room impulse response (a handful of decaying taps), delayed and attenuated.

use hearsay_attribution::normalize;

use crate::{Utterance, SAMPLE_RATE};

/// Tap offsets (ms after the direct path) and relative gains of the synthetic room response.
const RIR_TAPS: [(f64, f32); 5] = [
    (0.0, 1.0),
    (2.0, -0.5),
    (5.0, 0.35),
    (11.0, -0.2),
    (20.0, 0.1),
];

/// Loud-speech frame RMS the near-end and Them tracks are scaled to (about -22 dBFS), so an echo
/// level is relative to a realistic speech level rather than to the recording's gain.
pub const TARGET_LEVEL: f64 = 0.08;

/// Mic noise floor (RMS, about -66 dBFS).
pub const NOISE_RMS: f64 = 0.0005;

/// One speaker-to-mic path: playout plus acoustic delay and the echo's level relative to Them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EchoPath {
    pub delay_ms: f64,
    pub level_db: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// The near end is silent, so every Me word is spurious.
    EchoOnly,
    /// Near-end speech overlaps the echo.
    DoubleTalk,
    /// No Them audio at all.
    NearEndOnly,
}

impl Scenario {
    pub fn name(self) -> &'static str {
        match self {
            Scenario::EchoOnly => "echo-only",
            Scenario::DoubleTalk => "double-talk",
            Scenario::NearEndOnly => "near-end-only",
        }
    }
}

/// The stereo layout of a scenario: Me (left) is the synthetic mic, Them (right) the remote track.
#[derive(Debug, Clone)]
pub struct Mix {
    pub mic: Vec<f32>,
    pub them: Vec<f32>,
}

/// Sparse `(sample delay, gain)` taps of the echo path. The taps are scaled so the echo's RMS is
/// `level_db` relative to the Them signal's.
pub fn echo_taps(path: EchoPath) -> Vec<(usize, f32)> {
    let power: f64 = RIR_TAPS.iter().map(|(_, g)| f64::from(*g).powi(2)).sum();
    let scale = 10f64.powf(path.level_db / 20.0) / power.sqrt();
    RIR_TAPS
        .iter()
        .map(|(offset_ms, gain)| {
            let samples = ((path.delay_ms + offset_ms) * SAMPLE_RATE as f64 / 1000.0).round();
            (samples as usize, (f64::from(*gain) * scale) as f32)
        })
        .collect()
}

/// `them` through the taps, the same length as `them`.
pub fn render_echo(them: &[f32], taps: &[(usize, f32)]) -> Vec<f32> {
    let mut out = vec![0.0f32; them.len()];
    for &(delay, gain) in taps {
        for (i, sample) in them
            .iter()
            .enumerate()
            .take(them.len().saturating_sub(delay))
        {
            out[i + delay] += gain * sample;
        }
    }
    out
}

/// Deterministic zero-mean noise at `rms`.
pub fn noise(len: usize, rms: f64, seed: u64) -> Vec<f32> {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = f64::from((state >> 32) as u32) / f64::from(u32::MAX) - 0.5;
            (unit * 12f64.sqrt() * rms) as f32
        })
        .collect()
}

pub fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}

/// `samples` scaled by one gain so the 90th-percentile 20 ms frame RMS (the loud-speech level, not
/// thrown off by sparse clicks) is `target`, then clipped to full scale. All-zero input stays zero.
pub fn normalize_level(samples: &[f32], target: f64) -> Vec<f32> {
    let mut frames: Vec<f64> = samples.chunks(SAMPLE_RATE / 50).map(rms).collect();
    frames.sort_by(f64::total_cmp);
    let level = frames
        .get(frames.len() * 9 / 10)
        .copied()
        .unwrap_or_default();
    if level == 0.0 {
        return samples.to_vec();
    }
    let gain = (target / level) as f32;
    samples
        .iter()
        .map(|s| (s * gain).clamp(-1.0, 1.0))
        .collect()
}

/// Build one scenario from the clean `near` and `them` tracks (equal length).
pub fn build_mix(scenario: Scenario, near: &[f32], them: &[f32], path: EchoPath) -> Mix {
    let len = near.len().min(them.len());
    let floor = noise(len, NOISE_RMS, 1);
    let near_part = |i: usize| match scenario {
        Scenario::EchoOnly => 0.0,
        _ => near[i],
    };
    let (them_out, echo) = match scenario {
        Scenario::NearEndOnly => (vec![0.0; len], vec![0.0; len]),
        _ => (
            them[..len].to_vec(),
            render_echo(&them[..len], &echo_taps(path)),
        ),
    };
    let mic = (0..len)
        .map(|i| (near_part(i) + echo[i] + floor[i]).clamp(-1.0, 1.0))
        .collect();
    Mix {
        mic,
        them: them_out,
    }
}

fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|s| f64::from(*s).powi(2)).sum()
}

/// Echo return loss enhancement in dB: input energy over output energy, skipping the first
/// `skip` samples so the filter's initial adaptation does not dominate. Higher is better.
pub fn erle_db(mic: &[f32], out: &[f32], skip: usize) -> f64 {
    let n = mic.len().min(out.len());
    let skip = skip.min(n);
    10.0 * (energy(&mic[skip..n]).max(1e-12) / energy(&out[skip..n]).max(1e-12)).log10()
}

/// Second-order Butterworth high-pass at `cutoff_hz`. Recordings carry DC and rumble that the
/// canceller's built-in notch removes, which is not distortion of the speech.
pub fn highpass(samples: &[f32], cutoff_hz: f64) -> Vec<f32> {
    let w = 2.0 * std::f64::consts::PI * cutoff_hz / SAMPLE_RATE as f64;
    let alpha = w.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let cos = w.cos();
    let a0 = 1.0 + alpha;
    let b0 = (1.0 + cos) / 2.0 / a0;
    let b1 = -(1.0 + cos) / a0;
    let a1 = -2.0 * cos / a0;
    let a2 = (1.0 - alpha) / a0;
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    samples
        .iter()
        .map(|&s| {
            let x0 = f64::from(s);
            let y0 = b0 * x0 + b1 * x1 + b0 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, x0, y1, y0);
            y0 as f32
        })
        .collect()
}

const FFT_SIZE: usize = 512;
const FFT_HOP: usize = 256;
const BANDS: usize = 24;
/// Speech band scored for fidelity, as FFT bin indices (about 300 Hz to 7 kHz at 31.25 Hz a bin).
const BAND_LO_BIN: usize = 10;
const BAND_HI_BIN: usize = 224;

/// In-place radix-2 FFT; `re` and `im` have the same power-of-two length.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f64::consts::PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (sin, cos) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cos - im[b] * sin;
                let ti = re[b] * sin + im[b] * cos;
                (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                (re[a], im[a]) = (re[a] + tr, im[a] + ti);
            }
        }
        len <<= 1;
    }
}

/// Power per speech band for each Hann-windowed frame of `samples`: `[frame][band]`.
fn band_powers(samples: &[f32]) -> Vec<[f64; BANDS]> {
    let window: Vec<f64> = (0..FFT_SIZE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / FFT_SIZE as f64).cos())
        .collect();
    let edge = |b: usize| BAND_LO_BIN + b * (BAND_HI_BIN - BAND_LO_BIN) / BANDS;
    let mut frames = Vec::new();
    let mut start = 0;
    while start + FFT_SIZE <= samples.len() {
        let mut re: Vec<f64> = (0..FFT_SIZE)
            .map(|i| f64::from(samples[start + i]) * window[i])
            .collect();
        let mut im = vec![0.0; FFT_SIZE];
        fft(&mut re, &mut im);
        let mut bands = [0.0; BANDS];
        for (b, slot) in bands.iter_mut().enumerate() {
            *slot = (edge(b)..edge(b + 1))
                .map(|k| re[k] * re[k] + im[k] * im[k])
                .sum();
        }
        frames.push(bands);
        start += FFT_HOP;
    }
    frames
}

/// How well an estimate keeps the speech of a target, ignoring phase.
#[derive(Clone, Copy, Debug)]
pub struct Fidelity {
    /// Speech-band energy of the estimate relative to the target; below 0 the speech was attenuated.
    pub band_gain_db: f64,
    /// Correlation of the log band-energy tracks (per-band means removed): 1.0 is a perfect
    /// time-frequency match, near 0 is unrelated.
    pub envelope_corr: f64,
}

/// Compare `estimate` with `target` in the 300 Hz to 7 kHz band. The estimate may lag the target
/// by up to a few hundred samples (the preprocessor's one-frame latency); the better of lag 0 and
/// `lag` is scored.
pub fn fidelity(estimate: &[f32], target: &[f32], lag: usize) -> Fidelity {
    [0, lag]
        .into_iter()
        .map(|lag| {
            let estimate = &estimate[lag.min(estimate.len())..];
            let n = estimate.len().min(target.len());
            let (est, tgt) = (band_powers(&estimate[..n]), band_powers(&target[..n]));
            let total = |frames: &[[f64; BANDS]]| -> f64 { frames.iter().flatten().sum() };
            let log = |frames: &[[f64; BANDS]], b: usize| -> Vec<f64> {
                frames.iter().map(|f| (f[b] + 1e-9).ln()).collect()
            };
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for b in 0..BANDS {
                let (e, t) = (log(&est, b), log(&tgt, b));
                let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
                let (me, mt) = (mean(&e), mean(&t));
                xs.extend(e.iter().map(|v| v - me));
                ys.extend(t.iter().map(|v| v - mt));
            }
            let dot: f64 = xs.iter().zip(&ys).map(|(a, b)| a * b).sum();
            let norm = (xs.iter().map(|a| a * a).sum::<f64>()
                * ys.iter().map(|b| b * b).sum::<f64>())
            .sqrt()
            .max(1e-12);
            Fidelity {
                band_gain_db: 10.0 * (total(&est).max(1e-12) / total(&tgt).max(1e-12)).log10(),
                envelope_corr: dot / norm,
            }
        })
        .max_by(|a, b| a.envelope_corr.total_cmp(&b.envelope_corr))
        .expect("two lags")
}

/// Seconds until the canceller settles: the start of the first run of three consecutive active
/// one-second blocks whose ERLE is at least `threshold_db`. `None` when it never does.
pub fn convergence_s(mic: &[f32], out: &[f32], threshold_db: f64) -> Option<f64> {
    const ACTIVE_RMS: f64 = 0.002;
    let block = SAMPLE_RATE;
    let n = mic.len().min(out.len());
    let mut run: Vec<usize> = Vec::new();
    for start in (0..n.saturating_sub(block - 1)).step_by(block) {
        let m = &mic[start..start + block];
        if rms(m) < ACTIVE_RMS {
            continue;
        }
        if erle_db(m, &out[start..start + block], 0) >= threshold_db {
            run.push(start);
            if run.len() == 3 {
                return Some(run[0] as f64 / SAMPLE_RATE as f64);
            }
        } else {
            run.clear();
        }
    }
    None
}

/// Start of the `window_s` span, within `[0, total_s]`, that holds the most reference words in
/// utterances lying entirely inside it. Ties go to the earliest.
pub fn densest_window(utterances: &[Utterance], window_s: f64, total_s: f64) -> f64 {
    let latest = (total_s - window_s).max(0.0);
    let mut best = (0usize, 0.0f64);
    for candidate in utterances.iter().map(|u| u.start_s.min(latest)) {
        let words: usize = utterances
            .iter()
            .filter(|u| u.start_s >= candidate && u.end_s <= candidate + window_s)
            .map(|u| u.text.split_whitespace().count())
            .sum();
        if words > best.0 {
            best = (words, candidate);
        }
    }
    best.1
}

/// Utterances inside `[start_s, end_s]`, shifted so the window starts at zero.
pub fn slice_utterances(utterances: &[Utterance], start_s: f64, end_s: f64) -> Vec<Utterance> {
    utterances
        .iter()
        .filter(|u| u.start_s >= start_s && u.end_s <= end_s)
        .map(|u| Utterance {
            speaker: u.speaker.clone(),
            start_s: u.start_s - start_s,
            end_s: u.end_s - start_s,
            text: u.text.clone(),
        })
        .collect()
}

/// How many of the words in `finals` sit in a run of at least `RUN` consecutive words that also
/// occurs in `them_words`. A coarse, order-sensitive echo detector that ignores single common words.
pub fn echo_word_count(finals: &[Vec<String>], them_words: &[String]) -> usize {
    const RUN: usize = 3;
    let them_runs: std::collections::HashSet<&[String]> = them_words.windows(RUN).collect();
    let mut matched = 0;
    for words in finals {
        let mut covered = vec![false; words.len()];
        for (start, run) in words.windows(RUN).enumerate() {
            if them_runs.contains(run) {
                covered[start..start + RUN].fill(true);
            }
        }
        matched += covered.iter().filter(|c| **c).count();
    }
    matched
}

/// The normalized words of `texts`, one list per text.
pub fn normalized_words(texts: &[String]) -> Vec<Vec<String>> {
    texts.iter().map(|t| normalize(t)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(len: usize, freq: f64) -> Vec<f32> {
        (0..len)
            .map(|i| (0.3 * (i as f64 * freq).sin()) as f32)
            .collect()
    }

    fn utt(start_s: f64, end_s: f64, text: &str) -> Utterance {
        Utterance {
            speaker: "A".into(),
            start_s,
            end_s,
            text: text.into(),
        }
    }

    #[test]
    fn the_echo_is_delayed_and_scaled_to_the_requested_level() {
        let them = noise(SAMPLE_RATE * 4, 0.1, 7);
        let path = EchoPath {
            delay_ms: 40.0,
            level_db: -12.0,
        };
        let taps = echo_taps(path);
        assert_eq!(taps[0].0, 640, "40 ms at 16 kHz");
        let echo = render_echo(&them, &taps);
        assert!(
            echo[..640].iter().all(|s| *s == 0.0),
            "silent before the delay"
        );
        let measured = 20.0 * (rms(&echo) / rms(&them)).log10();
        assert!((measured + 12.0).abs() < 0.5, "echo level {measured} dB");
    }

    #[test]
    fn a_scenario_has_the_requested_parts() {
        let near = tone(16_000, 0.05);
        let them = tone(16_000, 0.21);
        let path = EchoPath {
            delay_ms: 40.0,
            level_db: -8.0,
        };

        let echo_only = build_mix(Scenario::EchoOnly, &near, &them, path);
        let echo = render_echo(&them, &echo_taps(path));
        let residual: Vec<f32> = echo_only
            .mic
            .iter()
            .zip(&echo)
            .map(|(m, e)| m - e)
            .collect();
        assert!(
            rms(&residual) < 2.0 * NOISE_RMS,
            "echo-only mic is echo plus floor"
        );
        assert_eq!(echo_only.them, them);

        let double = build_mix(Scenario::DoubleTalk, &near, &them, path);
        let near_part: Vec<f32> = double
            .mic
            .iter()
            .zip(&echo)
            .zip(&near)
            .map(|((m, e), n)| m - e - n)
            .collect();
        assert!(rms(&near_part) < 2.0 * NOISE_RMS);

        let alone = build_mix(Scenario::NearEndOnly, &near, &them, path);
        assert!(alone.them.iter().all(|s| *s == 0.0));
        let diff: Vec<f32> = alone.mic.iter().zip(&near).map(|(m, n)| m - n).collect();
        assert!(rms(&diff) < 2.0 * NOISE_RMS);
    }

    #[test]
    fn noise_is_deterministic_and_at_the_requested_rms() {
        assert_eq!(noise(100, 0.01, 3), noise(100, 0.01, 3));
        assert_ne!(noise(100, 0.01, 3), noise(100, 0.01, 4));
        let n = noise(100_000, 0.01, 3);
        assert!((rms(&n) - 0.01).abs() < 0.0005);
    }

    #[test]
    fn normalize_level_ignores_sparse_clicks_and_keeps_silence() {
        let mut speech = tone(SAMPLE_RATE * 2, 0.1);
        let scaled = normalize_level(&speech, TARGET_LEVEL);
        assert!((rms(&scaled) - TARGET_LEVEL).abs() < 1e-3);
        speech[100] = 1.0;
        speech[9000] = -1.0;
        let with_clicks = normalize_level(&speech, TARGET_LEVEL);
        assert!((rms(&with_clicks[20_000..]) - TARGET_LEVEL).abs() < 1e-3);
        assert!(with_clicks.iter().all(|s| s.abs() <= 1.0));
        assert!(normalize_level(&[0.0; 10], TARGET_LEVEL)
            .iter()
            .all(|s| *s == 0.0));
    }

    #[test]
    fn erle_is_the_energy_ratio_in_db() {
        let mic = vec![0.2f32; 1000];
        let out = vec![0.02f32; 1000];
        assert!((erle_db(&mic, &out, 0) - 20.0).abs() < 1e-6);
        assert!(erle_db(&mic, &mic, 0).abs() < 1e-6);
        let mixed: Vec<f32> = mic[..100].iter().chain(&out[100..]).copied().collect();
        assert!(
            (erle_db(&mic, &mixed, 100) - 20.0).abs() < 1e-6,
            "skip drops the head"
        );
    }

    #[test]
    fn highpass_removes_dc_and_keeps_speech_band() {
        let dc = highpass(&vec![0.5f32; 16_000], 100.0);
        assert!(dc[8_000..].iter().all(|s| s.abs() < 0.01));
        let tone: Vec<f32> = (0..16_000)
            .map(|i| (0.3 * (i as f64 * 0.4).sin()) as f32)
            .collect();
        let kept = highpass(&tone, 100.0);
        assert!((rms(&kept[2_000..]) / rms(&tone[2_000..]) - 1.0).abs() < 0.02);
    }

    fn bursts(len: usize, seed: u64) -> Vec<f32> {
        let carrier = noise(len, 0.1, seed);
        let envelope = noise(len / 800 + 1, 1.0, seed + 100);
        carrier
            .iter()
            .enumerate()
            .map(|(i, c)| c * (envelope[i / 800].abs() * 2.0))
            .collect()
    }

    #[test]
    fn the_fft_puts_a_tone_in_its_bin() {
        let mut re: Vec<f64> = (0..64)
            .map(|i| (2.0 * std::f64::consts::PI * 5.0 * f64::from(i) / 64.0).sin())
            .collect();
        let mut im = vec![0.0; 64];
        fft(&mut re, &mut im);
        let power: Vec<f64> = re.iter().zip(&im).map(|(r, i)| r * r + i * i).collect();
        let peak = (0..32)
            .max_by(|a, b| power[*a].total_cmp(&power[*b]))
            .unwrap();
        assert_eq!(peak, 5);
    }

    #[test]
    fn fidelity_tracks_level_and_time_frequency_match() {
        let target = bursts(SAMPLE_RATE * 8, 3);
        let same = fidelity(&target, &target, 160);
        assert!(same.band_gain_db.abs() < 1e-6 && same.envelope_corr > 0.999);

        let quiet: Vec<f32> = target.iter().map(|t| t * 0.5).collect();
        let half = fidelity(&quiet, &target, 160);
        assert!((half.band_gain_db + 6.0).abs() < 0.1 && half.envelope_corr > 0.999);

        let mut delayed = vec![0.0f32; 160];
        delayed.extend_from_slice(&target);
        assert!(fidelity(&delayed, &target, 160).envelope_corr > 0.99);

        let unrelated = fidelity(&bursts(SAMPLE_RATE * 8, 4), &target, 160);
        assert!(
            unrelated.envelope_corr < 0.3,
            "got {}",
            unrelated.envelope_corr
        );
    }

    #[test]
    fn convergence_is_the_start_of_three_good_blocks() {
        let mic = tone(SAMPLE_RATE * 8, 0.05);
        let mut out: Vec<f32> = mic.iter().map(|s| s * 0.01).collect();
        let loud_until = 3 * SAMPLE_RATE;
        for (o, m) in out.iter_mut().zip(&mic).take(loud_until) {
            *o = *m;
        }
        assert_eq!(convergence_s(&mic, &out, 10.0), Some(3.0));
        assert_eq!(convergence_s(&mic, &mic, 10.0), None);
    }

    #[test]
    fn the_densest_window_holds_the_most_words() {
        let utterances = vec![
            utt(0.0, 5.0, "a b"),
            utt(100.0, 105.0, "one two three four"),
            utt(110.0, 115.0, "five six seven eight"),
            utt(500.0, 505.0, "x"),
        ];
        assert_eq!(densest_window(&utterances, 30.0, 600.0), 100.0);
        assert_eq!(densest_window(&utterances, 30.0, 112.0), 82.0);
    }

    #[test]
    fn slicing_shifts_the_window_to_zero_and_keeps_whole_utterances() {
        let utterances = vec![
            utt(95.0, 99.0, "cut"),
            utt(100.0, 104.0, "in"),
            utt(125.0, 135.0, "cut"),
        ];
        let sliced = slice_utterances(&utterances, 100.0, 130.0);
        assert_eq!(sliced.len(), 1);
        assert_eq!(sliced[0].start_s, 0.0);
        assert_eq!(sliced[0].end_s, 4.0);
    }

    #[test]
    fn echo_words_are_those_in_runs_that_occur_in_them() {
        let them: Vec<String> = "we should ship the release on friday"
            .split(' ')
            .map(String::from)
            .collect();
        let finals = vec![
            "ship the release today"
                .split(' ')
                .map(String::from)
                .collect::<Vec<_>>(),
            "release the ship".split(' ').map(String::from).collect(),
        ];
        assert_eq!(echo_word_count(&finals, &them), 3);
    }
}
