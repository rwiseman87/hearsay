//! Test fixture: a minimal transcription sidecar for the `ProcessTranscriber` integration test.
//!
//! Speaks the same stdio contract as the real sidecars (and the Python `live_base.py`): emits the
//! `{"ready":true}` marker once "loaded", then reads `<u32 LE sample count><count * f32 LE>` feed
//! frames on stdin, emits one NDJSON `final` segment per frame on stdout, and on EOF emits a closing
//! `tail` segment then exits. Not shipped — it exists only so the crate's integration tests can
//! spawn a real process via `CARGO_BIN_EXE_*`.

use std::io::{self, Read, Write};

fn main() {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    // Signal readiness before serving, like the real sidecars once their models are loaded. The
    // read loop treats this as a non-segment line (it never counts as a transcript) and fires the
    // ready one-shot a spawn_warming() spawn hands the pipeline to gate its warm-up notice.
    writeln!(stdout, r#"{{"ready":true}}"#).unwrap();
    stdout.flush().unwrap();
    let mut index: u64 = 0;

    loop {
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).is_err() {
            break; // EOF: stdin closed
        }
        let count = u32::from_le_bytes(len_buf) as usize;
        let mut samples = vec![0u8; count * 4];
        if stdin.read_exact(&mut samples).is_err() {
            break;
        }
        let start = index as f64;
        writeln!(
            stdout,
            r#"{{"kind":"final","text":"chunk {index}","start_s":{start},"end_s":{}}}"#,
            start + 1.0
        )
        .unwrap();
        stdout.flush().unwrap();
        index += 1;
    }

    let start = index as f64;
    writeln!(
        stdout,
        r#"{{"kind":"final","text":"tail","start_s":{start},"end_s":{}}}"#,
        start + 1.0
    )
    .unwrap();
    stdout.flush().unwrap();
}
