//! Probe: decompose the macOS refine on a recorded meeting wav — channel extraction RMS, Swift
//! diarizer turns, and whisper's raw whole-track segments — so a bad refine can be blamed on the
//! right stage. Run:
//!   HEARSAY_BENCH_WAV=... HEARSAY_REFINE_MODEL=... HEARSAY_DIARIZE_BIN=... \
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --features metal \
//!     --test refine_mac_probe -- --ignored --nocapture

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hearsay_inference::{read_them_channel, Diarizer, SwiftDiarizer, WhisperAsr};

#[test]
#[ignore = "probe"]
fn refine_mac_probe() {
    let wav = PathBuf::from(std::env::var("HEARSAY_BENCH_WAV").expect("set HEARSAY_BENCH_WAV"));
    let model =
        PathBuf::from(std::env::var("HEARSAY_REFINE_MODEL").expect("set HEARSAY_REFINE_MODEL"));
    let diarize =
        PathBuf::from(std::env::var("HEARSAY_DIARIZE_BIN").expect("set HEARSAY_DIARIZE_BIN"));

    // 1. Extraction: the exact samples the refine feeds both stages, RMS per minute.
    let mut them = read_them_channel(&wav).expect("read them channel");
    // Optional slice (seconds), to probe a region in isolation.
    if let (Ok(a), Ok(b)) = (
        std::env::var("HEARSAY_PROBE_START_S"),
        std::env::var("HEARSAY_PROBE_END_S"),
    ) {
        let a: usize = a.parse().unwrap();
        let b: usize = b.parse().unwrap();
        them = them[a * 16_000..(b * 16_000).min(them.len())].to_vec();
        eprintln!("sliced to {a}-{b}s");
    }
    eprintln!(
        "extracted {} samples ({:.1}s)",
        them.len(),
        them.len() as f64 / 16_000.0
    );
    for (i, chunk) in them.chunks(60 * 16_000).enumerate() {
        let rms = (chunk
            .iter()
            .map(|&s| f64::from(s) * f64::from(s))
            .sum::<f64>()
            / chunk.len() as f64)
            .sqrt();
        eprint!("m{i}:{rms:.4} ");
    }
    eprintln!();

    // 2. The Swift diarizer on those samples (the app path writes them to a temp wav).
    let start = Instant::now();
    let diarization = SwiftDiarizer::new(&diarize, Duration::from_secs(300))
        .diarize(&them)
        .expect("diarize");
    let speakers: std::collections::HashSet<i64> =
        diarization.turns.iter().map(|t| t.speaker).collect();
    eprintln!(
        "diarizer: {} turns, {} speakers, last end {:.1}s ({:.1}s elapsed)",
        diarization.turns.len(),
        speakers.len(),
        diarization.turns.last().map_or(0.0, |t| t.end_s),
        start.elapsed().as_secs_f64()
    );

    // 3. Whisper whole-track, exactly as the refine runs it.
    let start = Instant::now();
    let mut asr = WhisperAsr::load(&model).expect("load whisper");
    if let Ok(thold) = std::env::var("HEARSAY_PROBE_ENTROPY") {
        asr = asr.with_entropy_thold(thold.parse().unwrap());
        eprintln!("entropy_thold = {thold}");
    }
    let segments = asr.transcribe(&them).expect("transcribe");
    let total_chars: usize = segments.iter().map(|s| s.text.len()).sum();
    eprintln!(
        "whisper: {} segments, {} chars, last end {:.1}s ({:.1}s elapsed)",
        segments.len(),
        total_chars,
        segments.last().map_or(0.0, |s| s.end_s),
        start.elapsed().as_secs_f64()
    );
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for seg in &segments {
        *counts.entry(seg.text.as_str()).or_insert(0) += 1;
    }
    let mut repeated: Vec<(&str, usize)> = counts.into_iter().filter(|&(_, n)| n > 3).collect();
    repeated.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (text, n) in repeated.iter().take(5) {
        eprintln!("repeated x{n}: {:?}", &text[..text.len().min(60)]);
    }
    for seg in segments.iter().take(6) {
        eprintln!(
            "  {:7.1}-{:7.1} {:?}",
            seg.start_s,
            seg.end_s,
            &seg.text[..seg.text.len().min(70)]
        );
    }
    eprintln!("  ...");
    for seg in segments
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        eprintln!(
            "  {:7.1}-{:7.1} {:?}",
            seg.start_s,
            seg.end_s,
            &seg.text[..seg.text.len().min(70)]
        );
    }
}
