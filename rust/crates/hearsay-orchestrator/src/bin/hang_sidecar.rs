//! Test fixture: a sidecar that drains stdin, then never closes stdout or exits, so tests can
//! prove `ProcessTranscriber::close()` is bounded.

use std::io::{self, Read};

fn main() {
    let mut buf = Vec::new();
    let _ = io::stdin().lock().read_to_end(&mut buf);
    // Wedge: never write to or close stdout, never exit. park() can wake spuriously, so loop.
    loop {
        std::thread::park();
    }
}
