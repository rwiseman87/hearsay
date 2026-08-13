//! Decode archived `audio.flac` files back to `audio.wav`, in place, under a recordings root.
//!
//! The inverse of the archival sweep, for getting a library back to uncompressed WAV. The FLAC is
//! left in place: this only ever adds a file, so a bad restore costs disk rather than audio.
//!
//! ```sh
//! cargo run --release -p hearsay-audio --example restore -- <recordings-dir> [--delete-flac]
//! ```

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: restore <recordings-dir> [--delete-flac]"),
    );
    let delete_flac = std::env::args().any(|a| a == "--delete-flac");

    let mut restored = 0usize;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(&root)
        .expect("read recordings dir")
        .flatten()
    {
        let dir = entry.path();
        let flac = dir.join(hearsay_audio::AUDIO_FLAC);
        let wav = dir.join(hearsay_audio::AUDIO_WAV);
        if !flac.is_file() || wav.exists() {
            continue;
        }
        match restore_one(&flac, &wav) {
            Ok(n) => {
                restored += 1;
                bytes += n;
                println!(
                    "restored {} ({n} bytes)",
                    dir.file_name().unwrap().to_string_lossy()
                );
                if delete_flac {
                    let _ = std::fs::remove_file(&flac);
                }
            }
            Err(e) => eprintln!("FAILED {}: {e}", dir.display()),
        }
    }
    println!(
        "restored {restored} meetings, {:.1} GiB",
        bytes as f64 / 1_073_741_824.0
    );
}

/// Decode `flac` to a temporary and rename it into place, so an interrupted run never leaves a
/// half-written `audio.wav` that would look like a real recording.
fn restore_one(flac: &Path, wav: &Path) -> Result<u64, String> {
    let channels = hearsay_audio::flac_channels(flac).map_err(|e| e.to_string())?;
    let spec = hound::WavSpec {
        channels: channels as u16,
        sample_rate: hearsay_audio::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let part = wav.with_extension("wav.part");
    let mut writer = hound::WavWriter::create(&part, spec).map_err(|e| e.to_string())?;
    hearsay_audio::for_each_flac_block(flac, |block| {
        for i in 0..block.duration() as usize {
            for ch in 0..channels {
                let _ = writer.write_sample(block.channel(ch as u32)[i] as i16);
            }
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    writer.finalize().map_err(|e| e.to_string())?;

    // Prove the restored wav re-encodes to the same audio before putting it in place.
    hearsay_audio::verify_flac_matches_wav(&part, flac).map_err(|e| format!("verify: {e}"))?;
    let n = std::fs::metadata(&part).map_err(|e| e.to_string())?.len();
    std::fs::rename(&part, wav).map_err(|e| e.to_string())?;
    Ok(n)
}
