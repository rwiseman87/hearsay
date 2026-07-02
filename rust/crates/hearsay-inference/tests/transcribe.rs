//! Opt-in accuracy smoke test against the real whisper model + the JFK clip in `outputs/`.
//! Ignored by default (needs the gitignored ~150 MB `ggml-base.bin` + is slow); run with:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference -- --ignored

use std::path::PathBuf;

use hearsay_inference::{read_wav_mono_16k, WhisperAsr};

/// Repo-root `outputs/` (this crate lives at `rust/crates/hearsay-inference`).
fn outputs(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../outputs")
        .join(rel)
}

#[test]
#[ignore = "needs outputs/models/ggml-base.bin + outputs/jfk.wav"]
fn transcribes_jfk_clip() {
    let samples = read_wav_mono_16k(outputs("jfk.wav")).unwrap();
    let asr = WhisperAsr::load(outputs("models/ggml-base.bin")).unwrap();
    let segments = asr.transcribe(&samples).unwrap();

    let text: String = segments
        .iter()
        .map(|s| s.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains("ask not what your country can do for you"),
        "unexpected transcript: {text}"
    );
}
