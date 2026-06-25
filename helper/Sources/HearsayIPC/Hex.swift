import Foundation

public enum Hex {
    public static func decode(_ s: String) -> [UInt8]? {
        guard s.count % 2 == 0 else { return nil }
        var out = [UInt8]()
        out.reserveCapacity(s.count / 2)
        var idx = s.startIndex
        while idx < s.endIndex {
            let next = s.index(idx, offsetBy: 2)
            guard let b = UInt8(s[idx ..< next], radix: 16) else { return nil }
            out.append(b)
            idx = next
        }
        return out
    }

    public static func encode(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}
