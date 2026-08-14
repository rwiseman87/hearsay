import Foundation

/// NDJSON control-channel messages (see `shared/protocol/ipc.md`).
///
/// Three shapes travel over `control.sock`, one UTF-8 JSON object per line:
/// - ``Command`` (core -> helper): `{"id", "cmd", "args"}`
/// - ``Reply``   (helper -> core): `{"id", "ok", "result"?|"error"?}` (correlates by `id`)
/// - ``Event``   (helper -> core): `{"event", "ts", "data"}` (unsolicited)

/// Error payload carried by a failed ``Reply``.
public struct ReplyError: Codable, Equatable, Sendable {
    public let code: String
    public let message: String

    public init(code: String, message: String) {
        self.code = code
        self.message = message
    }
}

/// A command from the core. `args` defaults to empty when the key is absent.
public struct Command: Codable, Equatable, Sendable {
    public let id: Int
    public let cmd: String
    public let args: [String: JSONValue]

    public init(id: Int, cmd: String, args: [String: JSONValue] = [:]) {
        self.id = id
        self.cmd = cmd
        self.args = args
    }

    private enum CodingKeys: String, CodingKey { case id, cmd, args }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        cmd = try c.decode(String.self, forKey: .cmd)
        args = try c.decodeIfPresent([String: JSONValue].self, forKey: .args) ?? [:]
    }
}

/// A reply to a command. Exactly one of `result` / `error` is present.
public struct Reply: Codable, Equatable, Sendable {
    public let id: Int
    public let ok: Bool
    public let result: [String: JSONValue]?
    public let error: ReplyError?

    private enum CodingKeys: String, CodingKey { case id, ok, result, error }

    public init(id: Int, ok: Bool, result: [String: JSONValue]?, error: ReplyError?) {
        self.id = id
        self.ok = ok
        self.result = result
        self.error = error
    }

    public static func ok(_ id: Int, _ result: [String: JSONValue] = [:]) -> Reply {
        Reply(id: id, ok: true, result: result, error: nil)
    }

    public static func fail(_ id: Int, code: String, message: String) -> Reply {
        Reply(id: id, ok: false, result: nil, error: ReplyError(code: code, message: message))
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(ok, forKey: .ok)
        try c.encodeIfPresent(result, forKey: .result)
        try c.encodeIfPresent(error, forKey: .error)
    }
}

/// An unsolicited event. `ts` shares the media `host_ts` clock.
public struct Event: Codable, Equatable, Sendable {
    public let event: String
    public let ts: UInt64
    public let data: [String: JSONValue]

    public init(event: String, ts: UInt64, data: [String: JSONValue] = [:]) {
        self.event = event
        self.ts = ts
        self.data = data
    }
}

/// Encodes/decodes a single NDJSON line for the control channel.
public enum ControlCodec {
    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return e
    }()

    /// Serialize a value to one NDJSON line (terminated by `\n`).
    public static func line<T: Encodable>(_ value: T) throws -> Data {
        var data = try encoder.encode(value)
        data.append(0x0A)
        return data
    }

    public static func decodeCommand(_ line: Data) throws -> Command {
        try JSONDecoder().decode(Command.self, from: line)
    }

    public static func decodeReply(_ line: Data) throws -> Reply {
        try JSONDecoder().decode(Reply.self, from: line)
    }

    public static func decodeEvent(_ line: Data) throws -> Event {
        try JSONDecoder().decode(Event.self, from: line)
    }
}

// MARK: - Ergonomic JSONValue literals (for building results/events)

extension JSONValue: ExpressibleByStringLiteral {
    public init(stringLiteral value: String) { self = .string(value) }
}

extension JSONValue: ExpressibleByIntegerLiteral {
    public init(integerLiteral value: Int64) { self = .int(value) }
}

extension JSONValue: ExpressibleByFloatLiteral {
    public init(floatLiteral value: Double) { self = .double(value) }
}

extension JSONValue: ExpressibleByBooleanLiteral {
    public init(booleanLiteral value: Bool) { self = .bool(value) }
}
