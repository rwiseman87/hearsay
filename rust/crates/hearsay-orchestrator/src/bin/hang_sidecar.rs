//! Test fixture: a wedged transcription sidecar that never closes stdout and never exits.
//!
//! It drains stdin to EOF (as a real sidecar would when the core drops its stdin) and then blocks
//! forever, keeping its stdout pipe open. Used to prove `ProcessTranscriber::close()` is bounded:
//! the parent's stdout drain must time out and kill the child rather than hang meeting stop. Not
//! shipped — spawned only via `CARGO_BIN_EXE_*` from the crate's integration tests.

use std::io::{self, Read};

fn main() {
    let mut buf = Vec::new();
    let _ = io::stdin().lock().read_to_end(&mut buf);
    // Wedge: never write to or close stdout, never exit. park() can wake spuriously, so loop.
    loop {
        std::thread::park();
    }
}
