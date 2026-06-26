import AVFoundation

enum PermissionState: String {
    case granted, denied, undetermined
}

/// TCC permission probing. Only the microphone has a side-effect-free status API;
/// audio capture is confirmed when the tap is built, and screen / accessibility /
/// calendar grants belong to later phases (reported `undetermined` for now).
enum Permissions {
    static func microphone() -> PermissionState {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return .granted
        case .denied, .restricted: return .denied
        case .notDetermined: return .undetermined
        @unknown default: return .undetermined
        }
    }

    /// Request microphone access, blocking until the user responds. Triggers the
    /// TCC prompt when the status is undetermined.
    static func requestMicrophone() -> PermissionState {
        let sem = DispatchSemaphore(value: 0)
        var granted = false
        AVCaptureDevice.requestAccess(for: .audio) {
            granted = $0
            sem.signal()
        }
        sem.wait()
        return granted ? .granted : .denied
    }

    /// The `permission` map for `check_permissions` / events.
    static func snapshot() -> [String: String] {
        [
            "microphone": microphone().rawValue,
            "audio_capture": PermissionState.undetermined.rawValue,
            "screen_recording": PermissionState.undetermined.rawValue,
            "accessibility": PermissionState.undetermined.rawValue,
            "calendar": PermissionState.undetermined.rawValue,
        ]
    }
}
