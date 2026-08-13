//! Write an archived-meeting fixture (`audio.flac` and nothing else) into a directory.
//!
//! Used by `make e2e` to give the browser suite a meeting whose audio exists only in the compressed
//! form, so the playback path is exercised end to end against a real encode rather than a stub.
//!
//! ```sh
//! cargo run -p hearsay-audio --example fixture -- outputs/e2e/fixture
//! ```

use std::path::PathBuf;

/// A couple of seconds is plenty to prove decode + duration + seeking, and keeps the file tiny.
const FRAMES: usize = 16_000 * 2;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: fixture <output-directory>"),
    );
    std::fs::create_dir_all(&dir).expect("create fixture dir");

    let spec = hound::WavSpec {
        channels: hearsay_audio::CHANNELS as u16,
        sample_rate: hearsay_audio::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let wav = dir.join(hearsay_audio::AUDIO_WAV);
    let flac = dir.join(hearsay_audio::AUDIO_FLAC);
    let _ = std::fs::remove_file(&flac);

    let mut writer = hound::WavWriter::create(&wav, spec).expect("create wav");
    for i in 0..FRAMES {
        // An audible tone per channel so the fixture is something a human can also play.
        let me = ((i as f32 * 0.08).sin() * 8_000.0) as i16;
        let them = ((i as f32 * 0.05).sin() * 10_000.0) as i16;
        writer.write_sample(me).expect("write sample");
        writer.write_sample(them).expect("write sample");
    }
    writer.finalize().expect("finalize wav");

    // Leaves only the flac behind, which is the state the sweep produces.
    let out = hearsay_audio::compress_meeting_audio(&dir).expect("compress fixture");
    println!(
        "{} ({} bytes, from {} bytes of wav)",
        flac.display(),
        out.flac_bytes,
        out.wav_bytes
    );
}
