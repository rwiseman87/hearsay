import Foundation

/// A bounded single-producer / single-consumer ring buffer of `Float` samples.
///
/// One capture source (audio thread) writes; the uplink thread drains. A single
/// `NSLock` guards the indices — the critical sections are tiny memcpy-style
/// copies, so contention is negligible at audio buffer sizes. On overrun the
/// oldest samples are discarded and counted (`droppedSamples`) so the uplink can
/// surface buffer pressure rather than blocking the audio thread.
final class RingBuffer: @unchecked Sendable {
    private var storage: [Float]
    private let capacity: Int
    private var head = 0  // next read
    private var count = 0
    private let lock = NSLock()
    private(set) var droppedSamples: UInt64 = 0

    init(capacity: Int) {
        precondition(capacity > 0)
        self.capacity = capacity
        self.storage = [Float](repeating: 0, count: capacity)
    }

    /// Append samples, dropping the oldest if the buffer would overflow.
    func write(_ samples: [Float]) {
        guard !samples.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }
        for s in samples {
            if count == capacity {
                head = (head + 1) % capacity  // drop oldest
                count -= 1
                droppedSamples &+= 1
            }
            storage[(head + count) % capacity] = s
            count += 1
        }
    }

    /// Pop up to `maxCount` samples (fewer if the buffer is near-empty).
    func read(upTo maxCount: Int) -> [Float] {
        lock.lock()
        defer { lock.unlock() }
        let n = Swift.min(maxCount, count)
        guard n > 0 else { return [] }
        var out = [Float](repeating: 0, count: n)
        for i in 0..<n {
            out[i] = storage[(head + i) % capacity]
        }
        head = (head + n) % capacity
        count -= n
        return out
    }

    var available: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }
}
