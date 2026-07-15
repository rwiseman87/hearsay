import Foundation
import os

/// Amplitude-based silence detector. The audio callback reports whether each buffer carried signal;
/// a watchdog reads how long the source has been continuously silent. Used for the microphone, whose
/// failure mode is a revoked TCC grant that still lets `AVAudioEngine` start and deliver *zeros*
/// (callbacks keep firing) — so silence, not callback cadence, is the tell. Its own lock is never held
/// across a device stop that drains the callback.
final class SilenceMonitor: @unchecked Sendable {
    private let lock = OSAllocatedUnfairLock(initialState: UInt64(0))  // 0 = currently receiving audio

    func record(nonZero: Bool, nowNs: UInt64) {
        lock.withLock { since in
            if nonZero {
                since = 0
            } else if since == 0 {
                since = nowNs
            }
        }
    }

    /// Nanoseconds of continuous silence, or 0 if audio is currently flowing.
    func silentForNs(nowNs: UInt64) -> UInt64 {
        lock.withLock { since in since == 0 ? 0 : nowNs &- since }
    }

    func reset() {
        lock.withLock { $0 = 0 }
    }
}

/// Audio-flow monitor. The tap's drain worker stamps the time each time it pulls fresh samples out of
/// the ring; the watchdog reads that timestamp against the current graph's start time. Used for the
/// system-audio tap, whose failure mode is a *stuck* tap — the real-time IOProc stops firing, so no
/// samples reach the ring — rather than firing with zeros. "Did audio flow at all" (cadence)
/// distinguishes a broken tap from legitimately quiet system audio, which amplitude cannot. The
/// watchdog compares against the graph start time (not a reset here) so a freshly rebuilt tap gets a
/// grace window and `recovered` fires only once real audio actually flows on the new graph.
final class FlowMonitor: @unchecked Sendable {
    private let lock: OSAllocatedUnfairLock<UInt64>

    init(nowNs: UInt64) {
        lock = OSAllocatedUnfairLock(initialState: nowNs)
    }

    func noteFlow(nowNs: UInt64) {
        lock.withLock { $0 = nowNs }
    }

    var lastFlowNs: UInt64 {
        lock.withLock { $0 }
    }
}

/// A one-way stop flag: set once by the owner, polled by a worker thread to know when to exit. Used to
/// tear down the tap's drain worker without the worker ever touching the tap's build lock (so teardown
/// can hold that lock while joining the worker).
final class StopFlag: @unchecked Sendable {
    private let flag = OSAllocatedUnfairLock(initialState: false)
    func set() { flag.withLock { $0 = true } }
    var isSet: Bool { flag.withLock { $0 } }
}
