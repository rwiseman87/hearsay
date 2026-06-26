import AVFoundation

/// Converts any input PCM format to the contract format: 16 kHz, mono, Float32.
///
/// `AVAudioConverter` is stateful across calls, which is exactly right for a
/// continuous stream: feed each incoming buffer and concatenate the outputs to
/// get a seamless 16 kHz signal. One instance is confined to a single capture
/// thread, so it needs no synchronization.
final class Resampler {
    static let targetFormat = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!

    private let converter: AVAudioConverter
    private let inputFormat: AVAudioFormat

    init?(from inputFormat: AVAudioFormat) {
        guard let c = AVAudioConverter(from: inputFormat, to: Self.targetFormat) else { return nil }
        self.inputFormat = inputFormat
        self.converter = c
    }

    /// Resample one input buffer to 16 kHz mono samples. Returns `[]` on error.
    func resample(_ input: AVAudioPCMBuffer) -> [Float] {
        guard input.frameLength > 0 else { return [] }
        let ratio = Self.targetFormat.sampleRate / inputFormat.sampleRate
        let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 32
        guard let out = AVAudioPCMBuffer(pcmFormat: Self.targetFormat, frameCapacity: capacity)
        else { return [] }

        var fed = false
        var nsErr: NSError?
        let status = converter.convert(to: out, error: &nsErr) { _, inStatus in
            if fed {
                inStatus.pointee = .noDataNow  // streaming: more input arrives next call
                return nil
            }
            fed = true
            inStatus.pointee = .haveData
            return input
        }
        guard status != .error, out.frameLength > 0, let ch = out.floatChannelData else { return [] }
        return Array(UnsafeBufferPointer(start: ch[0], count: Int(out.frameLength)))
    }
}
