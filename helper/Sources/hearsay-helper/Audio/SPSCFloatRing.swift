import os

/// Bounded single-producer / single-consumer ring of Float samples used to hand raw device-format
/// audio from the real-time Core Audio IOProc to a normal-priority worker thread. The IOProc (sole
/// producer) copies samples in; the worker (sole consumer) copies them out and resamples off the RT
/// thread.
///
/// An unfair lock guards `head`/`count`, but only the O(1) index update runs under it — the bulk
/// sample copy happens outside the lock. Two facts make that safe for SPSC:
///   - the producer's write region `(head + count) % cap` is invariant under a concurrent read (a read
///     advances `head` and lowers `count` by the same amount), and the producer only writes into free
///     slots (`writable <= capacity - count`), which a read can only grow;
///   - the consumer's read region `[head, head + n)` is filled data the producer never writes into.
/// So the real-time producer never waits on the consumer's copy — the dropout the tap restructure
/// removes. On overrun the newest samples that do not fit are dropped and counted.
///
/// (A fully lock-free ring would use stdlib `Atomic` head/tail, which needs macOS 15; the deployment
/// target is 14.4, so an unfair lock with an O(1) critical section is the pragmatic equivalent.)
final class SPSCFloatRing: @unchecked Sendable {
    private struct Indices {
        var head = 0
        var count = 0
        var dropped: UInt64 = 0
    }
    private let storage: UnsafeMutablePointer<Float>
    private let capacity: Int
    private let state = OSAllocatedUnfairLock(initialState: Indices())

    init(capacity: Int) {
        precondition(capacity > 0)
        self.capacity = capacity
        storage = UnsafeMutablePointer<Float>.allocate(capacity: capacity)
        storage.initialize(repeating: 0, count: capacity)
    }

    deinit {
        storage.deinitialize(count: capacity)
        storage.deallocate()
    }

    /// Producer (RT thread): append up to `count` samples from `src`, dropping (and counting) any that
    /// do not fit. Lock held only for the two O(1) index updates; the copy runs unlocked.
    func write(_ src: UnsafePointer<Float>, count: Int) {
        guard count > 0 else { return }
        let (start, writable) = state.withLock { s -> (Int, Int) in
            let free = capacity - s.count
            let n = Swift.min(count, free)
            if n < count { s.dropped &+= UInt64(count - n) }
            return ((s.head + s.count) % capacity, n)
        }
        guard writable > 0 else { return }
        copyIn(src, count: writable, at: start)
        state.withLock { s in s.count += writable }
    }

    /// Consumer (worker): copy up to `max` samples into `dst`, returning the number copied.
    func read(into dst: UnsafeMutablePointer<Float>, max: Int) -> Int {
        let (start, n) = state.withLock { s -> (Int, Int) in
            (s.head, Swift.min(max, s.count))
        }
        guard n > 0 else { return 0 }
        copyOut(to: dst, count: n, from: start)
        state.withLock { s in
            s.head = (s.head + n) % capacity
            s.count -= n
        }
        return n
    }

    var droppedSamples: UInt64 { state.withLock { $0.dropped } }

    // Wrap-aware bulk copies (at most 2 segments).
    private func copyIn(_ src: UnsafePointer<Float>, count: Int, at start: Int) {
        let first = Swift.min(count, capacity - start)
        (storage + start).update(from: src, count: first)
        if count > first { storage.update(from: src + first, count: count - first) }
    }

    private func copyOut(to dst: UnsafeMutablePointer<Float>, count: Int, from start: Int) {
        let first = Swift.min(count, capacity - start)
        dst.update(from: storage + start, count: first)
        if count > first { (dst + first).update(from: storage, count: count - first) }
    }
}
