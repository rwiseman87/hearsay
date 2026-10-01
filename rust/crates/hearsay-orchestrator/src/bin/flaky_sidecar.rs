//! Test fixture: a transcription sidecar that crashes mid-stream, for the respawn tests.
//!
//! Args: `<state_file> <crash_after_frames>`. If `state_file` is `-`, or does not exist yet (it is
//! then created), the process exits non-zero after emitting that many frames; otherwise it serves
//! normally, so only the first incarnation crashes. Each frame yields one final whose text is
//! `c<first sample as integer>` and whose times count samples from this process's first frame.
//! Not shipped — spawned only via `CARGO_BIN_EXE_*`.

use std::io::{self, Read, Write};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let state = &args[1];
    let crash_after: u64 = args[2].parse().unwrap();
    let crash = if state == "-" {
        true
    } else if Path::new(state).exists() {
        false
    } else {
        std::fs::write(state, b"crashed").unwrap();
        true
    };

    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    writeln!(stdout, r#"{{"ready":true}}"#).unwrap();
    stdout.flush().unwrap();

    let mut frames: u64 = 0;
    let mut samples_seen: u64 = 0;
    loop {
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).is_err() {
            break;
        }
        let count = u32::from_le_bytes(len_buf) as usize;
        let mut raw = vec![0u8; count * 4];
        if stdin.read_exact(&mut raw).is_err() {
            break;
        }
        let first = if count > 0 {
            f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as i64
        } else {
            0
        };
        let start = samples_seen as f64 / 16_000.0;
        samples_seen += count as u64;
        let end = samples_seen as f64 / 16_000.0;
        writeln!(
            stdout,
            r#"{{"kind":"final","text":"c{first}","start_s":{start},"end_s":{end}}}"#
        )
        .unwrap();
        stdout.flush().unwrap();
        frames += 1;
        if crash && frames >= crash_after {
            std::process::exit(1);
        }
    }
}
