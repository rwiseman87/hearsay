//! Windows capture-permission probe: microphone consent for desktop (Win32) apps is the global
//! "Let desktop apps access your microphone" toggle, stored in the CapabilityAccessManager
//! ConsentStore (there is no per-app prompt for unpackaged apps). System-audio loopback has no OS
//! permission gate, so `audio_capture` always reports granted. Registry reads only — nothing here
//! touches a device, so probing never triggers capture.

use std::path::PathBuf;

use windows_registry::CURRENT_USER;

use crate::PermissionsSnapshot;

/// The desktop-app (unpackaged/Win32) microphone consent toggle.
const NONPACKAGED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged";
/// The user-wide microphone toggle ("Microphone access"); Deny here blocks desktop apps too.
const GLOBAL_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// Read the ConsentStore microphone state. The `helper_path` parameter keeps the call site
/// platform-uniform (there is no helper process on Windows).
pub async fn probe_permissions(_helper_path: PathBuf) -> PermissionsSnapshot {
    PermissionsSnapshot {
        available: true,
        helper_version: None,
        microphone: Some(microphone_state()),
        // Loopback capture has no consent gate on Windows.
        audio_capture: Some("granted".to_string()),
    }
}

/// Map the two ConsentStore values onto the shared granted/denied/undetermined vocabulary: Deny
/// on either level blocks capture; Allow on the desktop-app toggle is granted; anything else
/// (missing keys, unexpected values) is undetermined.
fn microphone_state() -> String {
    let non_packaged = consent_value(NONPACKAGED_KEY);
    let global = consent_value(GLOBAL_KEY);
    match (non_packaged.as_deref(), global.as_deref()) {
        (Some("Deny"), _) | (_, Some("Deny")) => "denied".to_string(),
        (Some("Allow"), _) => "granted".to_string(),
        _ => "undetermined".to_string(),
    }
}

/// The `Value` string of a ConsentStore key (`Allow` / `Deny`), or `None` when unreadable.
fn consent_value(key: &str) -> Option<String> {
    CURRENT_USER.open(key).ok()?.get_string("Value").ok()
}
