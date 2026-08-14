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

    /// Resample `count` raw mono input-format frames (e.g. drained from the tap's SPSC ring, off the
    /// real-time thread) to 16 kHz. Wraps the samples in an input-format buffer and reuses `resample`.
    /// Assumes the input format is mono (one flat float channel), which the mono tap guarantees.
    func resample(_ frames: UnsafePointer<Float>, count: Int) -> [Float] {
        guard count > 0,
            let buf = AVAudioPCMBuffer(pcmFormat: inputFormat, frameCapacity: AVAudioFrameCount(count))
        else { return [] }
        buf.frameLength = AVAudioFrameCount(count)
        // Mono float: interleaved and deinterleaved layouts are identical, so copy raw bytes into the
        // first (only) buffer — works whether `inputFormat` reports interleaved or not.
        let abl = UnsafeMutableAudioBufferListPointer(buf.mutableAudioBufferList)
        if let mData = abl[0].mData {
            let bytes = Swift.min(Int(abl[0].mDataByteSize), count * MemoryLayout<Float>.stride)
            mData.copyMemory(from: frames, byteCount: bytes)
        }
        return resample(buf)
    }
}
