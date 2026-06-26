import Darwin

/// The single monotonic timebase that stamps both streams (`host_ts`).
///
/// `CLOCK_UPTIME_RAW` is `mach_absolute_time` already converted to nanoseconds:
/// monotonic, shared process-wide, and the clock the IPC contract names. Fusion
/// aligns the two streams by these values, never by sample index.
struct MonotonicClock: Sendable {
    func nowNs() -> UInt64 {
        clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
    }
}
