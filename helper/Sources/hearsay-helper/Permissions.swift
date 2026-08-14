import AVFoundation
import Darwin

enum PermissionState: String {
    case granted, denied, undetermined
}

/// `int TCCAccessPreflight(CFStringRef service, CFDictionaryRef options)` — returns the current TCC
/// status without prompting. Returns `int` (32-bit), so the C convention type must be `Int32`.
private typealias TCCAccessPreflightFn = @convention(c) (CFString, CFDictionary?) -> Int32

/// TCC permission probing. The microphone has a public side-effect-free status API; system-audio
/// capture (Core Audio process tap) does not, so it is read via the private `TCCAccessPreflight` SPI
/// (the approach Apple's AudioCap sample uses — this build is not sandboxed / not App Store).
enum Permissions {
    static func microphone() -> PermissionState {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return .granted
        case .denied, .restricted: return .denied
        case .notDetermined: return .undetermined
        @unknown default: return .undetermined
        }
    }

    /// System-audio (Core Audio process tap) TCC status, read side-effect-free via the private
    /// `TCCAccessPreflight` SPI with service `kTCCServiceAudioCapture` (this is the same permission the
    /// `NSAudioCaptureUsageDescription` prompt grants). Result: `0` granted, `1` denied, anything else
    /// undetermined. Degrades to `undetermined` if the private symbol can't be resolved.
    static func systemAudioCapture() -> PermissionState {
        guard
            let handle = dlopen(
                "/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC", RTLD_NOW)
        else {
            return .undetermined
        }
        defer { dlclose(handle) }
        guard let symbol = dlsym(handle, "TCCAccessPreflight") else {
            return .undetermined
        }
        let preflight = unsafeBitCast(symbol, to: TCCAccessPreflightFn.self)
        switch preflight("kTCCServiceAudioCapture" as CFString, nil) {
        case 0: return .granted
        case 1: return .denied
        default: return .undetermined
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
            "audio_capture": systemAudioCapture().rawValue,
        ]
    }
}
