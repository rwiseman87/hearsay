//! Shared path resolution for this crate's integration tests and probes.
//!
//! Compiled into each test binary that declares `mod common;`, so any given binary uses only part
//! of it.
#![allow(dead_code)]

use std::path::PathBuf;

/// Repo root joined with `rel`. This crate sits at `rust/crates/hearsay-inference`.
pub fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

/// Repo-root `outputs/` joined with `rel`.
pub fn outputs(rel: &str) -> PathBuf {
    repo("outputs").join(rel)
}

/// The sherpa model tree (`outputs/models/sherpa/`) joined with `rel`.
pub fn sherpa_model(rel: &str) -> PathBuf {
    outputs("models/sherpa").join(rel)
}

/// This crate's own directory joined with `rel`, for committed fixtures under `tests/`.
pub fn crate_rel(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}
