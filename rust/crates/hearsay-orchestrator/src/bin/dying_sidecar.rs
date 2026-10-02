//! Test fixture: a sidecar that exits immediately, like a failed model load, so tests can prove
//! a dead pooled sidecar is detected and replaced.

fn main() {
    std::process::exit(1);
}
