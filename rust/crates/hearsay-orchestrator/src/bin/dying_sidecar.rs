//! Test fixture: a sidecar that exits immediately, the way a real sidecar does when its model load
//! fails (`exit(1)`). Used to prove a pooled sidecar that died is detected via
//! `ProcessTranscriber::is_alive()`, so the warm pool evicts and re-spawns it rather than leaving a
//! dead pair to wedge the "Start" gate forever. Not shipped — spawned only via `CARGO_BIN_EXE_*`.

fn main() {
    std::process::exit(1);
}
