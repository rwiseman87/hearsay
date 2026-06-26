import AVFoundation

/// Captures the local microphone ("Me") via `AVAudioEngine`, resamples to the
/// contract format, and writes into a ``RingBuffer``. The mic is always "Me" and
/// is never diarized.
///
/// `AVAudioEngine` stops itself on an audio-configuration change (input/output
/// device or sample-rate change) and must be restarted. A configuration-change
/// observer rebuilds the resampler for the new input format, reinstalls the tap,
/// and restarts the engine — the mic-side counterpart to ``SystemAudioTap``'s
/// watchdog — emitting `mic_health` so a device change never silently kills "Me".
final class MicCapture: AudioSource, @unchecked Sendable {
    private let ring: RingBuffer
    private let onHealth: (_ state: String, _ action: String?) -> Void
    private let log: (String) -> Void
    private let engine = AVAudioEngine()
    private let lock = NSLock()
    private let configQueue = DispatchQueue(label: "hearsay.mic.config")
    private var running = false
    // Set before the tap is installed, read on the audio thread. Reassigned only
    // while the engine is stopped (start / config-change rebuild, both under
    // `lock`), so the audio callback never races a live reassignment.
    private var resampler: Resampler?
    private var configObserver: NSObjectProtocol?

    init(
        ring: RingBuffer,
        onHealth: @escaping (_ state: String, _ action: String?) -> Void,
        log: @escaping (String) -> Void
    ) {
        self.ring = ring
        self.onHealth = onHealth
        self.log = log
    }

    func start() throws {
        lock.lock()
        defer { lock.unlock() }
        guard !running else { return }
        try startEngineLocked()
        running = true
        configObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil
        ) { [weak self] _ in
            self?.configQueue.async { self?.handleConfigChange() }
        }
    }

    func stop() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }
        running = false
        if let obs = configObserver {
            NotificationCenter.default.removeObserver(obs)
            configObserver = nil
        }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
    }

    /// Install the resampling tap (input -> 16 kHz mono) and start the engine.
    /// Caller holds `lock` and has removed any previously installed tap.
    private func startEngineLocked() throws {
        let input = engine.inputNode
        let inFormat = input.inputFormat(forBus: 0)
        guard inFormat.sampleRate > 0, inFormat.channelCount > 0 else {
            throw CaptureError.noInputFormat
        }
        guard let rs = Resampler(from: inFormat) else { throw CaptureError.resamplerInit }
        resampler = rs

        input.installTap(onBus: 0, bufferSize: 1024, format: inFormat) { [weak self] buf, _ in
            guard let self, let rs = self.resampler else { return }
            let samples = rs.resample(buf)
            if !samples.isEmpty { self.ring.write(samples) }
        }
        engine.prepare()
        try engine.start()
    }

    /// Rebuild after AVAudioEngine stopped on an audio-config change (the new input
    /// format may differ, so the resampler is rebuilt from it).
    private func handleConfigChange() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }  // a concurrent stop() won the race
        log("mic config changed -> restarting engine")
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        do {
            try startEngineLocked()
            onHealth("recovered", "restarted_engine")
        } catch {
            onHealth("degraded", nil)
            log("mic restart failed: \(error)")
        }
    }
}
