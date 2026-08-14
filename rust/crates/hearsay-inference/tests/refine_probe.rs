#![cfg(feature = "sherpa")]
//! Probe: run the Windows refine (sherpa pyannote diarizer + whisper) exactly as
//! `hearsay-backends`' `WindowsRefiner` does, on a wav given by `HEARSAY_BENCH_WAV`. Reproduces a
//! refine crash outside the app, where the panic/assert is visible. Run:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --release \
//!     --features sherpa,vulkan refine_probe -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use hearsay_inference::{refine_audio_file_with, SherpaDiarizer};

#[test]
#[ignore = "probe"]
fn refine_probe() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let sherpa = repo.join("outputs/models/sherpa");
    let wav = PathBuf::from(std::env::var("HEARSAY_BENCH_WAV").expect("set HEARSAY_BENCH_WAV"));
    let model = std::env::var("HEARSAY_REFINE_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo.join("outputs/models/ggml-small.en.bin"));

    let segmentation = sherpa.join("sherpa-onnx-pyannote-segmentation-3-0/model.onnx");
    let embedding = sherpa.join("nemo_en_titanet_small.onnx");
    eprintln!("wav:   {}", wav.display());
    eprintln!("model: {}", model.display());

    let start = Instant::now();
    let diarizer = SherpaDiarizer::load(&segmentation, &embedding).expect("load diarizer");
    eprintln!("diarizer loaded in {:.2}s", start.elapsed().as_secs_f64());

    let start = Instant::now();
    let out = refine_audio_file_with(&wav, &diarizer, &model).expect("refine");
    eprintln!(
        "refine: {:.2}s, {} segments",
        start.elapsed().as_secs_f64(),
        out.segments.len()
    );
    for seg in out.segments.iter().take(4) {
        eprintln!("  [{:>6.2}] spk {}  {}", seg.start_s, seg.ordinal, seg.text);
    }
}
