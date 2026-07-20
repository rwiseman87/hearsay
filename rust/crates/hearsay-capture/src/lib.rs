//! Cross-platform audio capture behind the orchestrator's `AudioSource` trait.
//!
//! **macOS ([`SwiftHelperSource`], `swift_helper`):** reuse the proven Swift `hearsay-helper` — the
//! only process that touches the guarded Core Audio tap (Them) + mic (Me); the core pumps its
//! socket traffic into `CaptureChunk`s (`shared/protocol/ipc.md`).
//!
//! **Windows (`WasapiSource`, in progress — see `docs/windows-port.md`):** in-process WASAPI
//! capture, no helper process. Me = the default capture endpoint; Them = system-audio loopback,
//! [`LoopbackMode`] selecting classic device loopback (default) or process-loopback-exclude-self.
//!
//! `AudioSource` itself lives in `hearsay-orchestrator`; this crate implements it, plus the
//! per-OS permissions probe behind the shared [`PermissionsSnapshot`].

#[cfg(target_os = "macos")]
mod swift_helper;
mod synthetic;
#[cfg(windows)]
mod wasapi_source;

#[cfg(target_os = "macos")]
pub use swift_helper::{probe_permissions, SwiftHelperSource};
pub use synthetic::SyntheticSource;
#[cfg(windows)]
pub use wasapi_source::WasapiSource;

/// Which WASAPI path captures the Them (system audio) stream on Windows
/// (`HEARSAY_WIN_LOOPBACK`). Unused on macOS, where the Core Audio tap is always
/// global-except-self.
///
/// `Device` (the default) is classic loopback on the default render endpoint: captures everything
/// that plays — including new-Teams meetings, which the process-loopback API currently records as
/// silence (microsoft/Windows-classic-samples#414) — at the cost of not excluding Hearsay's own
/// playback. `Process` is process-loopback with `EXCLUDE_TARGET_PROCESS_TREE` on our own PID, the
/// exact global-except-self analog, selectable for when the Teams bug is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopbackMode {
    #[default]
    Device,
    Process,
}

impl std::str::FromStr for LoopbackMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "device" => Ok(Self::Device),
            "process" => Ok(Self::Process),
            _ => Err(format!(
                "unknown loopback mode {s:?} (expected \"device\" or \"process\")"
            )),
        }
    }
}

/// A per-OS capture-permission snapshot for the Permissions panel. Never an error: an unavailable
/// prober yields `available = false` with every field `None` (the API renders those as
/// `"unknown"`). On macOS the fields are the helper's TCC states; on Windows only `microphone`
/// (the desktop-app consent toggle) and `audio_capture` (no OS gate on loopback) apply.
#[derive(Debug, Clone, Default)]
pub struct PermissionsSnapshot {
    pub available: bool,
    pub helper_version: Option<String>,
    pub microphone: Option<String>,
    pub audio_capture: Option<String>,
    pub screen_recording: Option<String>,
    pub accessibility: Option<String>,
    pub calendar: Option<String>,
}

/// Non-macOS placeholder probe (the Windows ConsentStore read lands with the Windows backend):
/// degrade to an unavailable snapshot so the Permissions panel renders. The `helper_path`
/// parameter keeps the call site platform-uniform; there is no helper off macOS.
#[cfg(not(target_os = "macos"))]
pub async fn probe_permissions(_helper_path: std::path::PathBuf) -> PermissionsSnapshot {
    PermissionsSnapshot::default()
}

#[cfg(test)]
mod tests {
    use super::LoopbackMode;

    #[test]
    fn loopback_mode_parses_known_values_case_insensitively() {
        assert_eq!("device".parse::<LoopbackMode>(), Ok(LoopbackMode::Device));
        assert_eq!(
            " Process ".parse::<LoopbackMode>(),
            Ok(LoopbackMode::Process)
        );
        assert!("global".parse::<LoopbackMode>().is_err());
    }
}
