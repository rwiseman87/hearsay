/// A capture source that produces 16 kHz mono `Float` samples into a ``RingBuffer``.
///
/// Implementations (`MicCapture`, `SystemAudioTap`, `SyntheticSource`) own their
/// target ring and resample to the contract format before writing. The uplink
/// thread stamps `host_ts` as it drains, so sources only deliver samples.
protocol AudioSource: AnyObject, Sendable {
    func start() throws
    func stop()
}
