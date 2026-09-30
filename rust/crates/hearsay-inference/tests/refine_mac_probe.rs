//! Probe: decompose the macOS refine on a recorded meeting wav — channel extraction RMS, Swift
//! diarizer turns, and whisper's raw whole-track segments — so a bad refine can be blamed on the
//! right stage. Run:
//!   HEARSAY_BENCH_WAV=... HEARSAY_REFINE_MODEL=... HEARSAY_DIARIZE_BIN=... \
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --features metal \
//!     --test refine_mac_probe -- --ignored --nocapture

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hearsay_inference::{
    read_them_channel, refine_them_with, Diarizer, SwiftDiarizer, WhisperAsr, LOOP_MIN_CYCLES,
};

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
    if std::env::var("HEARSAY_PROBE_CARRY_OVER").as_deref() == Ok("0") {
        asr = asr.with_carry_over(false);
        eprintln!("carry_over = off");
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
    // Optional full dump (`start<TAB>end<TAB>text`), so a bad region can be read in context rather
    // than inferred from the head/tail excerpt below.
    if let Ok(path) = std::env::var("HEARSAY_PROBE_DUMP") {
        let dump: String = segments
            .iter()
            .map(|s| format!("{:.2}\t{:.2}\t{}\n", s.start_s, s.end_s, s.text))
            .collect();
        std::fs::write(&path, dump).expect("write dump");
        eprintln!("dumped {} segments to {path}", segments.len());
    }

    // Back-to-back repeats, not total occurrences: "Yeah." a dozen times across half an hour is
    // speech, the same sentence a dozen times in a row is the decoder looping. Counting occurrences
    // instead let a 120-of-783 loop pass this probe.
    let mut runs: Vec<(&str, usize)> = Vec::new();
    for seg in &segments {
        match runs.last_mut() {
            Some((text, n)) if *text == seg.text => *n += 1,
            _ => runs.push((seg.text.as_str(), 1)),
        }
    }
    runs.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (text, n) in runs.iter().take(5).filter(|&&(_, n)| n > 1) {
        eprintln!("consecutive x{n}: {:?}", &text[..text.len().min(60)]);
    }

    assert!(!them.is_empty(), "extraction produced no Them samples");
    assert!(!segments.is_empty(), "whisper produced no segments");
    assert!(total_chars > 0, "whisper produced empty transcript");
    let (worst_text, max_run) = runs.first().copied().unwrap_or(("", 0));
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

    // 4. The assembled refine output — what replace_them_segments would store.
    let refined = refine_them_with(
        &asr,
        &SwiftDiarizer::new(&diarize, Duration::from_secs(300)),
        &them,
    )
    .expect("refine");
    let mut per_ordinal: HashMap<i64, usize> = HashMap::new();
    for seg in &refined.segments {
        *per_ordinal.entry(seg.ordinal).or_insert(0) += 1;
    }
    let longest = refined
        .segments
        .iter()
        .map(|s| s.end_s - s.start_s)
        .fold(0.0f64, f64::max);
    eprintln!(
        "assembled: {} segments, per-ordinal {:?}, longest {:.1}s",
        refined.segments.len(),
        per_ordinal,
        longest
    );
    assert!(
        !refined.segments.is_empty(),
        "refine assembled no Them segments from a non-empty transcript"
    );
    // Asserted last so a failing run still reports every stage above it. A real meeting produces
    // speech, not a decoder repetition loop, and this guards both halves of the anti-loop defence in
    // asr.rs (see hearsay-refine-performance): the entropy_thold that catches a short looping phrase
    // mid-decode, and the loop repair that re-decodes what the entropy gate is structurally blind to
    // — a repeated unit of 32+ tokens (one meeting came back with the same sentence 120 times,
    // another with one phrase 473 times).
    assert!(
        max_run < LOOP_MIN_CYCLES,
        "whisper repetition loop survived the refine: {:?} repeated {max_run}x in a row \
         of {} segments (loop repair or entropy_thold regressed?)",
        &worst_text[..worst_text.len().min(60)],
        segments.len()
    );
}
