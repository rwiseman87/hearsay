import Darwin
import Foundation

public enum SocketError: Error, Equatable {
    case socketFailed(Int32)
    case connectFailed(Int32)
    case writeFailed(Int32)
    case readFailed(Int32)
    case pathTooLong
}

/// Blocking `AF_UNIX` / `SOCK_STREAM` client.
///
/// The helper connects to the two sockets the core listens on (`control.sock`,
/// `media.sock`). Blocking I/O on dedicated threads keeps the capture path simple;
/// a file descriptor is safe to write from one thread while another reads it.
public final class UnixSocketClient: @unchecked Sendable {
    public let fd: Int32

    public init(path: String) throws {
        let f = socket(AF_UNIX, SOCK_STREAM, 0)
        guard f >= 0 else { throw SocketError.socketFailed(errno) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        let pathBytes = path.utf8
        guard pathBytes.count < capacity else {
            Darwin.close(f)
            throw SocketError.pathTooLong
        }
        withUnsafeMutablePointer(to: &addr.sun_path) { raw in
            raw.withMemoryRebound(to: CChar.self, capacity: capacity) { dst in
                _ = strncpy(dst, path, capacity - 1)
            }
        }

        let rc = withUnsafePointer(to: &addr) { p -> Int32 in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                connect(f, sa, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard rc == 0 else {
            let e = errno
            Darwin.close(f)
            throw SocketError.connectFailed(e)
        }
        self.fd = f
    }

    /// Bound blocking writes with `SO_SNDTIMEO`. A stalled peer (the core stops reading) then makes
    /// `write` fail with `EAGAIN` after `seconds` instead of blocking forever — so a caller holding a
    /// lock across a write (the media uplink) cannot wedge the shutdown path. Best-effort: a failed
    /// `setsockopt` leaves the socket in its default blocking mode.
    public func setWriteTimeout(seconds: Double) {
        var tv = timeval(
            tv_sec: Int(seconds),
            tv_usec: Int32((seconds - Double(Int(seconds))) * 1_000_000))
        _ = setsockopt(
            fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
    }

    /// Write every byte, retrying short writes and `EINTR`.
    public func writeAll(_ data: Data) throws {
        try data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            guard let base = raw.baseAddress else { return }
            var off = 0
            while off < raw.count {
                let n = Darwin.write(fd, base + off, raw.count - off)
                if n > 0 {
                    off += n
                } else if n < 0 && errno == EINTR {
                    continue
                } else {
                    throw SocketError.writeFailed(errno)
                }
            }
        }
    }

    public func writeAll(_ bytes: [UInt8]) throws {
        try writeAll(Data(bytes))
    }

    /// Read up to `maxLength` bytes. Returns an empty array on EOF.
    public func read(maxLength: Int = 65536) throws -> [UInt8] {
        var buf = [UInt8](repeating: 0, count: maxLength)
        while true {
            let n = buf.withUnsafeMutableBytes { p in
                Darwin.read(fd, p.baseAddress, maxLength)
            }
            if n > 0 { return Array(buf[0..<n]) }
            if n == 0 { return [] }
            if errno == EINTR { continue }
            throw SocketError.readFailed(errno)
        }
    }

    public func close() {
        Darwin.close(fd)
    }
}

/// Splits a byte stream into `\n`-terminated lines (NDJSON framing).
///
/// One reader thread owns an instance; not safe for concurrent use.
public final class LineReader: @unchecked Sendable {
    private let sock: UnixSocketClient
    private var buffer: [UInt8] = []

    public init(_ sock: UnixSocketClient) { self.sock = sock }

    /// Block until a full line is available. Returns `nil` at EOF (a trailing
    /// partial line without a newline is discarded, per the line-framed contract).
    public func nextLine() throws -> Data? {
        while true {
            if let idx = buffer.firstIndex(of: 0x0A) {
                let line = Data(buffer[0..<idx])
                buffer.removeFirst(idx + 1)
                return line
            }
            let chunk = try sock.read()
            if chunk.isEmpty { return nil }
            buffer.append(contentsOf: chunk)
        }
    }
}
