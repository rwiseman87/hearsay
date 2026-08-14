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
///
/// Unlike the tap, the mic's other failure mode is a revoked TCC grant or a stranded input endpoint:
/// `engine.start()` still succeeds but delivers *digital silence* (exact zeros), so "Me" is silently
/// empty. A silence watchdog detects a sustained run of exact-zero buffers (a live mic always carries
/// a noise floor above the tiny threshold), emits `mic_health degraded` / `recovered` edge-triggered,
/// and restarts the engine with backoff — the endpoint can strand without ever posting a configuration
/// change, so reporting alone would leave "Me" dead for the rest of the meeting.
final class MicCapture: AudioSource, @unchecked Sendable {
    private let ring: RingBuffer
    private let onHealth: (_ state: String, _ action: String?) -> Void
    private let log: (String) -> Void
    private let engine = AVAudioEngine()
    private let lock = NSLock()
    private let configQueue = DispatchQueue(label: "hearsay.mic.config")
    private let clock = MonotonicClock()
    private var running = false
    // Set before the tap is installed, read on the audio thread. Reassigned only
    // while the engine is stopped (start / config-change rebuild, both under
    // `lock`), so the audio callback never races a live reassignment.
    private var resampler: Resampler?
    private var configObserver: NSObjectProtocol?

    // Silence watchdog: detects a mic delivering only zeros (revoked TCC / hardware mute).
    private let silence = SilenceMonitor()
    private var silenceTimer: DispatchSourceTimer?
    private var micSilent = false  // edge-trigger state for `mic_health`
    // Callback-cadence monitor. The tap block fires on every buffer even during silence, so a mic that
    // stops *delivering* buffers (engine wedged, no config-change) is invisible to the amplitude-based
    // `silence` watchdog above (which sees no zeros, only absence). Stamped per buffer, surfaced by the
    // telemetry line, so that stall mode is diagnosable rather than silent.
    private let callback: FlowMonitor
    private var lastTelemetryNs: UInt64 = 0
    private let telemetryThrottleNs: UInt64 = 5_000_000_000  // one telemetry line every 5 s
    // Below this, a sample counts as silence; a live mic's noise floor sits well above it, so only a
    // truly dead input (exact zeros) trips the watchdog — not a quiet user.
    private let silenceFloor: Float = 1e-6
    private let silenceThresholdNs: UInt64 = 8_000_000_000  // 8 s of continuous silence
    // A stranded input endpoint delivers zeros indefinitely without ever posting a configuration
    // change, so the config-change observer never fires and the engine would stay dead for the rest of
    // the meeting. The silence watchdog restarts it itself, backing off so a mic that is legitimately
    // muted at the hardware level does not churn the engine once per second.
    private var restartBackoffNs: UInt64 = 0
    private var nextRestartAtNs: UInt64 = 0
    private let maxRestartBackoffNs: UInt64 = 30_000_000_000

    init(
        ring: RingBuffer,
        onHealth: @escaping (_ state: String, _ action: String?) -> Void,
        log: @escaping (String) -> Void
    ) {
        self.ring = ring
        self.onHealth = onHealth
        self.log = log
        self.callback = FlowMonitor(nowNs: clock.nowNs())
    }

    func start() throws {
        lock.lock()
        defer { lock.unlock() }
        guard !running else { return }
        try startEngineLocked()
        running = true
        micSilent = false
        silence.reset()
        configObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil
        ) { [weak self] _ in
            self?.configQueue.async { self?.handleConfigChange() }
        }
        installSilenceTimerLocked()
    }

    func stop() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }
        running = false
        silenceTimer?.cancel()
        silenceTimer = nil
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
            guard !samples.isEmpty else { return }
            var nonZero = false
            for s in samples where abs(s) > self.silenceFloor {
                nonZero = true
                break
            }
            let nowNs = self.clock.nowNs()
            self.callback.noteFlow(nowNs: nowNs)  // cadence: a buffer arrived, regardless of content
            self.silence.record(nonZero: nonZero, nowNs: nowNs)
            self.ring.write(samples)
        }
        engine.prepare()
        try engine.start()
    }

    /// Start the once-per-second silence watchdog (caller holds `lock`). Runs on `configQueue`, so it
    /// serializes with the config-change restart and never races it.
    private func installSilenceTimerLocked() {
        guard silenceTimer == nil else { return }
        let timer = DispatchSource.makeTimerSource(queue: configQueue)
        timer.schedule(deadline: .now() + 1.0, repeating: 1.0)
        timer.setEventHandler { [weak self] in self?.checkSilence() }
        timer.resume()
        silenceTimer = timer
    }

    /// Emit `mic_health` edge-triggered: `degraded` once the mic has delivered only silence past the
    /// threshold, `recovered` once audio flows again — and restart the engine, backoff-paced, for as
    /// long as the silence lasts.
    private func checkSilence() {
        let now = clock.nowNs()
        let silentNs = silence.silentForNs(nowNs: now)
        let callbackAgeNs = now &- callback.lastFlowNs
        lock.lock()
        var emit: String?
        var logTelemetry = false
        var wantRestart = false
        if running {
            if silentNs >= silenceThresholdNs {
                if !micSilent {
                    micSilent = true
                    emit = "degraded"
                    nextRestartAtNs = 0  // restart immediately on first detection
                }
                if now >= nextRestartAtNs {
                    wantRestart = true
                    restartBackoffNs =
                        restartBackoffNs == 0
                        ? 1_000_000_000 : min(restartBackoffNs * 2, maxRestartBackoffNs)
                    nextRestartAtNs = now &+ restartBackoffNs
                }
            } else if micSilent {
                micSilent = false
                restartBackoffNs = 0
                nextRestartAtNs = 0
                emit = "recovered"
            }
            if now &- lastTelemetryNs >= telemetryThrottleNs {
                lastTelemetryNs = now
                logTelemetry = true
            }
        }
        lock.unlock()
        if let state = emit { onHealth(state, nil) }
        if wantRestart { restartSilentEngine() }
        // Both liveness axes on one line: `silent_ms` (amplitude) rises when the mic delivers zeros;
        // `callback_age_ms` (cadence) rises when it stops delivering buffers at all — the stall the
        // silence watchdog can't see. Emitted outside the lock so a slow log never blocks the timer.
        if logTelemetry {
            log(
                "mic telemetry: callback_age_ms=\(callbackAgeNs / 1_000_000) "
                    + "silent_ms=\(silentNs / 1_000_000)")
        }
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
            micSilent = false
            silence.reset()  // fresh input; do not carry stale silence across the restart
            onHealth("recovered", "restarted_engine")
        } catch {
            onHealth("degraded", nil)
            log("mic restart failed: \(error)")
        }
    }

    /// Restart the engine after a sustained run of digital silence. Runs on `configQueue`, so it
    /// serializes with ``handleConfigChange``.
    ///
    /// Deliberately does *not* reset the silence monitor: the restart is a repair attempt, not
    /// evidence of one. Carrying the silence forward keeps `recovered` meaning "real audio came back"
    /// and lets the backoff keep retrying if this restart landed on the same dead endpoint.
    private func restartSilentEngine() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }  // a concurrent stop() won the race
        log("mic delivering digital silence -> restarting engine")
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        do {
            try startEngineLocked()
        } catch {
            log("mic silence restart failed: \(error)")
        }
    }
}
