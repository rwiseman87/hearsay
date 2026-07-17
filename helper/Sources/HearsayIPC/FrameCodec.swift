import Foundation

/// Frame type codes. Raw values are the on-wire bytes; `wire` is the JSON/string form.
public enum FrameType: UInt8, Sendable {
    case audio = 0
    case hello = 1
    case heartbeat = 2
    case eos = 3

    public var wire: String {
        switch self {
        case .audio: return "audio"
        case .hello: return "hello"
        case .heartbeat: return "heartbeat"
        case .eos: return "eos"
        }
    }
}

/// Capture channel. `me` = local microphone, `them` = system audio output.
public enum StreamKind: UInt8, Sendable {
    case me = 0
    case them = 1

    public var wire: String { self == .me ? "me" : "them" }
}

public enum SampleFormat: UInt8, Sendable {
    case int16 = 0
    case float32 = 1

    public var wire: String { self == .int16 ? "int16" : "float32" }
    public var bytesPerSample: Int { self == .int16 ? 2 : 4 }
}

public enum ProtocolError: Error, Equatable {
    case tooShort
    case badMagic(UInt8)
    case badVersion(UInt8)
    case unknownCode(String)
    case truncated
    case badPayload
}

/// One media frame. `nSamples` is derived from the payload (mirrors the Rust `hearsay-ipc` codec).
public struct MediaFrame: Equatable, Sendable {
    public let type: FrameType
    public let stream: StreamKind
    public let format: SampleFormat
    public let seq: UInt32
    public let hostTs: UInt64
    public let payload: [UInt8]
    public let flags: UInt8

    public init(
        type: FrameType,
        stream: StreamKind,
        format: SampleFormat,
        seq: UInt32,
        hostTs: UInt64,
        payload: [UInt8] = [],
        flags: UInt8 = 0
    ) {
        self.type = type
        self.stream = stream
        self.format = format
        self.seq = seq
        self.hostTs = hostTs
        self.payload = payload
        self.flags = flags
    }

    public var nSamples: UInt32 {
        type == .audio ? UInt32(payload.count / format.bytesPerSample) : 0
    }
}

/// Byte-for-byte mirror of `hearsay.helper.protocol` (see shared/protocol/ipc.md).
public enum FrameCodec {
    public static let magic: UInt8 = 0xA7
    public static let version: UInt8 = 1
    public static let headerSize = 28

    public static func encode(_ f: MediaFrame) throws -> [UInt8] {
        if f.type == .audio {
            if f.payload.count % f.format.bytesPerSample != 0 { throw ProtocolError.badPayload }
        } else if !f.payload.isEmpty {
            throw ProtocolError.badPayload
        }
        var out = [UInt8]()
        out.reserveCapacity(headerSize + f.payload.count)
        out.append(magic)
        out.append(version)
        out.append(f.type.rawValue)
        out.append(f.stream.rawValue)
        out.append(f.format.rawValue)
        out.append(f.flags)
        appendLE16(&out, 0)  // reserved0
        appendLE32(&out, f.seq)
        appendLE64(&out, f.hostTs)
        appendLE32(&out, f.nSamples)
        appendLE32(&out, 0)  // reserved1
        out.append(contentsOf: f.payload)
        return out
    }

    public static func decode(_ buf: [UInt8]) throws -> MediaFrame {
        guard buf.count >= headerSize else { throw ProtocolError.tooShort }
        guard buf[0] == magic else { throw ProtocolError.badMagic(buf[0]) }
        guard buf[1] == version else { throw ProtocolError.badVersion(buf[1]) }
        guard let type = FrameType(rawValue: buf[2]) else {
            throw ProtocolError.unknownCode("type \(buf[2])")
        }
        guard let stream = StreamKind(rawValue: buf[3]) else {
            throw ProtocolError.unknownCode("stream \(buf[3])")
        }
        guard let format = SampleFormat(rawValue: buf[4]) else {
            throw ProtocolError.unknownCode("format \(buf[4])")
        }
        let flags = buf[5]
        let seq = readLE32(buf, 8)
        let hostTs = readLE64(buf, 12)
        let nSamples = readLE32(buf, 20)
        let payloadLen = type == .audio ? Int(nSamples) * format.bytesPerSample : 0
        guard buf.count >= headerSize + payloadLen else { throw ProtocolError.truncated }
        let payload = Array(buf[headerSize ..< headerSize + payloadLen])
        return MediaFrame(
            type: type, stream: stream, format: format,
            seq: seq, hostTs: hostTs, payload: payload, flags: flags
        )
    }

    // MARK: - little-endian helpers

    static func appendLE16(_ out: inout [UInt8], _ v: UInt16) {
        out.append(UInt8(v & 0xff))
        out.append(UInt8((v >> 8) & 0xff))
    }

    static func appendLE32(_ out: inout [UInt8], _ v: UInt32) {
        for i in 0 ..< 4 { out.append(UInt8((v >> (8 * i)) & 0xff)) }
    }

    static func appendLE64(_ out: inout [UInt8], _ v: UInt64) {
        for i in 0 ..< 8 { out.append(UInt8((v >> (8 * i)) & 0xff)) }
    }

    static func readLE32(_ b: [UInt8], _ o: Int) -> UInt32 {
        UInt32(b[o]) | (UInt32(b[o + 1]) << 8) | (UInt32(b[o + 2]) << 16) | (UInt32(b[o + 3]) << 24)
    }

    static func readLE64(_ b: [UInt8], _ o: Int) -> UInt64 {
        var v: UInt64 = 0
        for i in 0 ..< 8 { v |= UInt64(b[o + i]) << (8 * i) }
        return v
    }
}
