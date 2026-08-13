//! Repair the STREAMINFO frame-size fields of already-archived FLACs.
//!
//! Files written before the encoder populated `min_frame_size` / `max_frame_size` carry
//! `min = 0xFFFFFF` (flacenc's "no frame seen yet" sentinel) and `max = 0` -- a contradictory range
//! that AVFoundation refuses to play, even though the audio itself is correct. Rewriting the six
//! bytes to the spec's "unknown" (0/0) restores playback without touching a sample.
//!
//! ```sh
//! cargo run --release -p hearsay-audio --example repair_header -- <recordings-dir>
//! ```

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// STREAMINFO payload starts at byte 8 ("fLaC" + a 4-byte metadata block header); within it,
/// min_frame_size is a 24-bit field at offset 4 and max_frame_size at offset 7.
const MIN_FRAME_OFFSET: u64 = 12;

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: repair_header <recordings-dir>"),
    );
    let (mut repaired, mut ok, mut failed) = (0usize, 0usize, 0usize);
    for entry in std::fs::read_dir(&root).expect("read dir").flatten() {
        let flac = entry.path().join(hearsay_audio::AUDIO_FLAC);
        if !flac.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        match repair(&flac) {
            Ok(true) => {
                repaired += 1;
                println!("repaired  {name}");
            }
            Ok(false) => {
                ok += 1;
                println!("already ok {name}");
            }
            Err(e) => {
                failed += 1;
                eprintln!("FAILED    {name}: {e}");
            }
        }
    }
    println!("{repaired} repaired, {ok} already ok, {failed} failed");
}

/// Returns whether the file needed repair. Only rewrites when the declared range is impossible, and
/// re-reads the file afterwards to confirm it still decodes.
fn repair(path: &Path) -> Result<bool, String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut header = [0u8; 6];
    file.seek(SeekFrom::Start(MIN_FRAME_OFFSET))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    let min = u32::from_be_bytes([0, header[0], header[1], header[2]]);
    let max = u32::from_be_bytes([0, header[3], header[4], header[5]]);
    // A max of 0 means "unknown", which is only coherent when min is also 0.
    if !(max == 0 && min > 0) {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(MIN_FRAME_OFFSET))
        .map_err(|e| e.to_string())?;
    file.write_all(&[0u8; 6]).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);

    // The audio is untouched, but prove the file still decodes end to end before calling it done.
    let channels = hearsay_audio::flac_channels(path).map_err(|e| e.to_string())?;
    let mut frames = 0u64;
    hearsay_audio::for_each_flac_block(path, |b| {
        frames += b.duration() as u64;
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    if frames == 0 || channels == 0 {
        return Err("decoded no audio after repair".into());
    }
    Ok(true)
}
