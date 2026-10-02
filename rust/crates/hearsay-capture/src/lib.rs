//! Audio capture behind the orchestrator's `AudioSource` trait.
//!
//! [`SwiftHelperSource`] (`swift_helper`) reuses the proven Swift `hearsay-helper` — the only process
//! that touches the guarded Core Audio tap (Them) + mic (Me); the core pumps its socket traffic into
//! `CaptureChunk`s (`shared/protocol/ipc.md`).
//!
//! `AudioSource` itself lives in `hearsay-orchestrator`; this crate implements it, plus the
//! permissions probe behind the shared [`PermissionsSnapshot`].

#[cfg(target_os = "macos")]
mod swift_helper;

#[cfg(target_os = "macos")]
pub use swift_helper::{probe_permissions, SwiftHelperSource};

/// A capture-permission snapshot for the Permissions panel. Never an error: an unavailable prober
/// yields `available = false` with every field `None` (the API renders those as `"unknown"`). The
/// fields are the helper's TCC states.
#[derive(Debug, Clone, Default)]
pub struct PermissionsSnapshot {
    pub available: bool,
    pub helper_version: Option<String>,
    pub microphone: Option<String>,
    pub audio_capture: Option<String>,
}
