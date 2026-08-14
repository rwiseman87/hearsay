import Foundation

/// A deterministic test source: a continuous sine tone at the contract format.
///
/// It exercises the entire ring-buffer -> uplink -> `media.sock` path without any
/// TCC permission or real audio device, so the IPC pipe is verifiable in CI and
/// off-device. Each stream is given a distinct frequency so the two `.wav` files
/// produced by the capture-debug reader are visibly different.
final class SyntheticSource: AudioSource, @unchecked Sendable {
    private let ring: RingBuffer
    private let frequency: Double
    private let amplitude: Float
    private let sampleRate = 16_000.0
    private let clock = MonotonicClock()

    private let lock = NSLock()
    private var running = false
    private var thread: Thread?
    private var phase = 0.0  // touched only by the generator thread

    init(ring: RingBuffer, frequency: Double, amplitude: Float = 0.2) {
        self.ring = ring
        self.frequency = frequency
        self.amplitude = amplitude
    }

    func start() throws {
        lock.lock()
        running = true
        lock.unlock()
        let t = Thread { [weak self] in self?.run() }
        t.name = "synthetic-source-\(Int(frequency))"
        thread = t
        t.start()
    }

    func stop() {
        lock.lock()
        running = false
        lock.unlock()
    }

    private func isRunning() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return running
    }

    private func run() {
        let increment = 2.0 * .pi * frequency / sampleRate
        let twoPi = 2.0 * Double.pi
        let startNs = clock.nowNs()
        var produced = 0
        while isRunning() {
            // Emit exactly enough samples to track real time, so the synthetic
            // stream advances at 16 kHz wall-clock like a real capture device.
            let elapsedNs = clock.nowNs() &- startNs
            let target = Int(Double(elapsedNs) / 1_000_000_000.0 * sampleRate)
            let need = target - produced
            if need > 0 {
                var chunk = [Float](repeating: 0, count: need)
                for i in 0..<need {
                    chunk[i] = amplitude * Float(sin(phase))
                    phase += increment
                    if phase > twoPi { phase -= twoPi }
                }
                ring.write(chunk)
                produced += need
            }
            Thread.sleep(forTimeInterval: 0.01)  // ~10 ms cadence
        }
    }
}
