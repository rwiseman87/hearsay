//! `hearsay-inference <model.bin> <audio-16k.wav>` — transcribe a clip and print its segments +
//! timing. The manual accuracy-verification tool: run it against known audio on the Mac (e.g.
//! `outputs/models/ggml-base.bin outputs/jfk.wav`).

use std::path::PathBuf;

use hearsay_inference::{read_wav_mono_16k, WhisperAsr, SAMPLE_RATE};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: {} <model.bin> <audio-16k.wav>", args[0]);
        std::process::exit(2);
    }
    let model = PathBuf::from(&args[1]);
    let wav = PathBuf::from(&args[2]);

    let samples = read_wav_mono_16k(&wav)?;
    let audio_s = samples.len() as f64 / SAMPLE_RATE as f64;

    let asr = WhisperAsr::load(&model)?;
    let start = std::time::Instant::now();
    let segments = asr.transcribe(&samples)?;
    let elapsed = start.elapsed().as_secs_f64();

    for seg in &segments {
        println!("[{:>7.2} -> {:>7.2}]  {}", seg.start_s, seg.end_s, seg.text);
    }
    eprintln!(
        "\n{} segments · {:.1}s audio · {:.2}s compute · {:.1}x realtime",
        segments.len(),
        audio_s,
        elapsed,
        if elapsed > 0.0 {
            audio_s / elapsed
        } else {
            0.0
        }
    );
    Ok(())
}
