import CoreAudio
import Darwin

/// Errors raised while building the capture graph.
enum CaptureError: Error, CustomStringConvertible {
    case noInputFormat
    case resamplerInit
    case createTap(OSStatus)
    case tapFormat
    case createAggregate(OSStatus)
    case createIOProc(OSStatus)
    case startDevice(OSStatus)

    var description: String {
        switch self {
        case .noInputFormat: return "input node has no usable format"
        case .resamplerInit: return "could not create resampler"
        case .createTap(let s): return "AudioHardwareCreateProcessTap failed (\(s))"
        case .tapFormat: return "could not read tap stream format"
        case .createAggregate(let s): return "AudioHardwareCreateAggregateDevice failed (\(s))"
        case .createIOProc(let s): return "AudioDeviceCreateIOProcIDWithBlock failed (\(s))"
        case .startDevice(let s): return "AudioDeviceStart failed (\(s))"
        }
    }
}

/// Read the tap's stream format (`kAudioTapPropertyFormat`).
func tapStreamFormat(_ tapID: AudioObjectID) -> AudioStreamBasicDescription? {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioTapPropertyFormat,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var asbd = AudioStreamBasicDescription()
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    let st = AudioObjectGetPropertyData(tapID, &addr, 0, nil, &size, &asbd)
    return st == noErr ? asbd : nil
}

/// Read a fixed-size `AudioObjectID` property (no qualifier).
func caGetObjectID(
    _ objectID: AudioObjectID, _ selector: AudioObjectPropertySelector
) -> AudioObjectID? {
    var addr = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var value = AudioObjectID(kAudioObjectUnknown)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let st = AudioObjectGetPropertyData(objectID, &addr, 0, nil, &size, &value)
    return st == noErr ? value : nil
}

/// Translate this process's PID to its Core Audio process object, so it can be
/// excluded from a global tap ("global-except-self").
func selfAudioProcessObject() -> AudioObjectID? {
    var pid = getpid()
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var obj = AudioObjectID(kAudioObjectUnknown)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let st = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &addr,
        UInt32(MemoryLayout<pid_t>.size), &pid, &size, &obj)
    return (st == noErr && obj != AudioObjectID(kAudioObjectUnknown)) ? obj : nil
}

/// The system's current default output device (for watchdog listeners).
func defaultOutputDevice() -> AudioObjectID? {
    caGetObjectID(
        AudioObjectID(kAudioObjectSystemObject),
        kAudioHardwarePropertyDefaultOutputDevice)
}
