#![cfg(feature = "sherpa")]
//! Opt-in streaming-ASR benchmark: measures load time, one-shot RTF, and live per-chunk latency for
//! any streaming transducer, so a candidate model can be judged against the real-time budget before
//! it is adopted. Two streams (Me + Them) run concurrently in a meeting, so the usable ceiling is
//! roughly half the single-stream headroom. Ignored by default (needs a model on disk). Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --release --features sherpa \
//!     streaming_bench -- --ignored --nocapture
//!
//! Override the defaults with `HEARSAY_BENCH_MODEL_DIR` (a dir holding encoder/decoder/joiner
//! `.onnx` plus `tokens.txt`) and `HEARSAY_BENCH_WAV` (16 kHz mono).

mod common;
use common::repo;

use std::path::{Path, PathBuf};
use std::time::Instant;

use hearsay_inference::{read_wav_mono_16k, StreamingAsr, StreamingModel};

/// The first file in `dir` whose name contains `stem` and ends in `.onnx`.
fn onnx(dir: &Path, stem: &str) -> PathBuf {
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read model dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            name.contains(stem) && name.ends_with(".onnx")
        })
        .collect();
    hits.sort();
    hits.into_iter()
        .next()
        .unwrap_or_else(|| panic!("no *{stem}*.onnx in {}", dir.display()))
}

#[test]
#[ignore = "needs a streaming model on disk"]
fn streaming_bench() {
    let dir = std::env::var("HEARSAY_BENCH_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            repo("outputs/models/sherpa/sherpa-onnx-streaming-zipformer-en-2023-06-21")
        });
    // No default: the sherpa model archives do not all ship a sample clip, and a silent-by-default
    // benchmark would report a meaningless RTF.
    let wav = PathBuf::from(
        std::env::var("HEARSAY_BENCH_WAV")
            .expect("set HEARSAY_BENCH_WAV to a 16 kHz mono wav to benchmark against"),
    );

    eprintln!("model: {}", dir.display());
    eprintln!("wav:   {}", wav.display());

    let load_start = Instant::now();
    let asr = StreamingAsr::load(StreamingModel {
        encoder: &onnx(&dir, "encoder"),
        decoder: &onnx(&dir, "decoder"),
        joiner: &onnx(&dir, "joiner"),
        tokens: &dir.join("tokens.txt"),
    })
    .expect("load streaming asr");
    let load_s = load_start.elapsed().as_secs_f64();

    let samples = read_wav_mono_16k(&wav).expect("read wav");
    let audio_s = samples.len() as f64 / 16_000.0;

    // One-shot: total compute for the whole clip.
    let start = Instant::now();
    let text = asr.transcribe(&samples);
    let oneshot_s = start.elapsed().as_secs_f64();

    // Live: feed in 560 ms chunks (the model's advertised chunk) and record per-chunk latency, the
    // number that decides whether partials keep up in real time.
    let chunk = 8_960; // 560 ms at 16 kHz
    let mut session = asr.session();
    let mut per_chunk_ms: Vec<f64> = Vec::new();
    let live_start = Instant::now();
    for block in samples.chunks(chunk) {
        let t = Instant::now();
        let _ = session.feed(block);
        per_chunk_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let _ = session.finish();
    let live_s = live_start.elapsed().as_secs_f64();

    per_chunk_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = per_chunk_ms[per_chunk_ms.len() / 2];
    let worst = *per_chunk_ms.last().unwrap();
    let budget_ms = chunk as f64 / 16.0; // 560 ms of audio per chunk

    eprintln!("\n--- results ---");
    eprintln!("model load:        {load_s:.2} s");
    eprintln!("audio:             {audio_s:.2} s");
    eprintln!(
        "one-shot compute:  {oneshot_s:.2} s  (RTF {:.3})",
        oneshot_s / audio_s
    );
    eprintln!(
        "streaming compute: {live_s:.2} s  (RTF {:.3})",
        live_s / audio_s
    );
    eprintln!("per-chunk median:  {median:.0} ms of a {budget_ms:.0} ms budget");
    eprintln!("per-chunk worst:   {worst:.0} ms");
    eprintln!(
        "two-stream RTF:    {:.3}  <-- must stay well under 1.0",
        2.0 * live_s / audio_s
    );
    eprintln!("\ntranscript: {text}");

    assert!(
        !text.trim().is_empty(),
        "empty transcript — model did not decode"
    );
}
