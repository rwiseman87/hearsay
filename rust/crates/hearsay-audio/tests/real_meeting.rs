//! Archival probe against a real recorded meeting. Ignored by default (it needs a recording that
//! only exists on a machine that has run the app); run it to confirm the compression ratio and the
//! losslessness claim on genuine capture rather than synthesized audio:
//!
//! ```sh
//! HEARSAY_ARCHIVE_PROBE_WAV=~/path/to/audio.wav \
//!   cargo test --release -p hearsay-audio --test real_meeting -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Instant;

#[test]
#[ignore = "needs a real recorded meeting (set HEARSAY_ARCHIVE_PROBE_WAV)"]
fn compresses_a_real_meeting_losslessly() {
    let Some(src) = std::env::var_os("HEARSAY_ARCHIVE_PROBE_WAV").map(PathBuf::from) else {
        panic!("set HEARSAY_ARCHIVE_PROBE_WAV to a recorded meeting's audio.wav");
    };
    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join(hearsay_audio::AUDIO_WAV);
    std::fs::copy(&src, &wav).expect("copy the probe recording");

    let started = Instant::now();
    let out = hearsay_audio::compress_meeting_audio(tmp.path()).expect("compress");
    let elapsed = started.elapsed();

    let ratio = out.wav_bytes as f64 / out.flac_bytes as f64;
    let minutes = out.wav_bytes as f64 / (16_000.0 * 2.0 * 2.0) / 60.0;
    eprintln!(
        "{:.1} min: {:.1} MiB -> {:.1} MiB ({ratio:.2}x) in {:.1}s",
        minutes,
        out.wav_bytes as f64 / 1_048_576.0,
        out.flac_bytes as f64 / 1_048_576.0,
        elapsed.as_secs_f64(),
    );

    // compress_meeting_audio only removes the wav after a byte-exact verify, so reaching here is
    // itself the losslessness proof; this just pins the saving being worth the work.
    assert!(ratio > 2.0, "expected a real saving, got {ratio:.2}x");
    assert!(!wav.exists());
}
