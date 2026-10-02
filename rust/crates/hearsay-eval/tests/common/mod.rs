//! Shared setup for the live-pipeline evals (`crash_eval`, `echo_eval`): the AMI tracks, the paced
//! WAV backend, and sidecar warm-up.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hearsay_eval::echo::{densest_window, highpass, normalize_level, TARGET_LEVEL};
use hearsay_eval::{load_utterances, resolve_audio, Utterance, SAMPLE_RATE};
use hearsay_inference::read_wav_mono_16k;
use hearsay_orchestrator::{Backend, BackendInstance, ProcessTranscriber, WavFileSource};

pub const THEM_AUDIO: &str = "ami/ES2004a.Mix-Headset.wav";
pub const THEM_TRANSCRIPT: &str = "ES2004a.utterances.json";
pub const NEAR_AUDIO: &str = "ami/ES2004b.Headset-0.wav";
pub const NEAR_TRANSCRIPT: &str = "ES2004b-A.utterances.json";
/// Recordings carry DC and low-frequency rumble a real mic chain removes.
pub const MIC_HIGHPASS_HZ: f64 = 100.0;
pub const TAIL_SILENCE_S: usize = 3;
pub const READY_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Sidecars {
    pub me: PathBuf,
    pub live: PathBuf,
}

/// The Them and near-end tracks cut to their densest `window_s` window, level-normalized.
pub struct Tracks {
    pub them: Vec<f32>,
    pub near: Vec<f32>,
    #[allow(dead_code)] // read by echo_eval only
    pub them_utts: Vec<Utterance>,
    pub near_utts: Vec<Utterance>,
    pub them_start: f64,
    pub near_start: f64,
    pub window_s: f64,
}

/// A positive number from `name`, else `default`.
pub fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/// Load both AMI tracks and their transcripts; `None` (with a printed reason) when audio is absent.
pub fn load_tracks(label: &str, window_s: f64) -> Option<Tracks> {
    let them_path = resolve_audio(THEM_AUDIO);
    let near_path = resolve_audio(NEAR_AUDIO);
    for path in [&them_path, &near_path] {
        if !path.exists() {
            eprintln!("{label}: audio absent ({}); skipping", path.display());
            return None;
        }
    }
    let them_full = read_wav_mono_16k(&them_path).expect("read Them audio");
    let near_full = read_wav_mono_16k(&near_path).expect("read near-end audio");
    let secs = |n: usize| n as f64 / SAMPLE_RATE as f64;
    let window_s = window_s
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
    Some(Tracks {
        them: prepare(&them_full, them_start),
        near: prepare(&near_full, near_start),
        them_utts,
        near_utts,
        them_start,
        near_start,
        window_s,
    })
}

/// Write `me` (left) and `them` (right) as a float stereo WAV with trailing silence.
pub fn write_stereo(path: &Path, me: &[f32], them: &[f32]) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create stereo wav");
    let frames = me.len().max(them.len()) + TAIL_SILENCE_S * SAMPLE_RATE;
    for i in 0..frames {
        writer
            .write_sample(me.get(i).copied().unwrap_or(0.0))
            .unwrap();
        writer
            .write_sample(them.get(i).copied().unwrap_or(0.0))
            .unwrap();
    }
    writer.finalize().unwrap();
}

/// Feeds a WAV at `speed` to two pre-warmed transcribers, once.
pub struct RunBackend {
    pub wav: PathBuf,
    pub speed: f64,
    pub me: Mutex<Option<ProcessTranscriber>>,
    pub them: Mutex<Option<ProcessTranscriber>>,
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

/// Spawn a sidecar warming, optionally with a VAD threshold override.
pub fn warm(binary: &Path, vad: Option<f64>) -> ProcessTranscriber {
    let mut transcriber = ProcessTranscriber::new(binary.to_path_buf());
    if let Some(threshold) = vad {
        transcriber = transcriber.with_env("HEARSAY_VAD_THRESHOLD", &threshold.to_string());
    }
    transcriber.spawn_warming().expect("spawn sidecar");
    transcriber
}

pub async fn wait_ready(me: &ProcessTranscriber, them: &ProcessTranscriber) {
    let deadline = Instant::now() + READY_TIMEOUT;
    while !(me.is_ready() && them.is_ready()) {
        assert!(Instant::now() < deadline, "sidecars never became ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A value to two decimals, or `-` when absent.
pub fn fmt_opt(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |v| format!("{v:.2}"))
}
