//
//  AnyCodable.swift
//  SandboxedDashboard
//
//  Helper type for decoding arbitrary JSON values
//

import Foundation

/// `@unchecked Sendable` because `value: Any` defeats the Sendable check, but
/// in practice the `init(from:)` path only ever stores Foundation primitives
/// (Bool/Int/Double/String, NSNull, or recursive `[AnyCodable]` / `[String: AnyCodable]`).
/// Once decoded, the value is treated as immutable. Required so structures
/// that transitively contain `AnyCodable` (StoredEvent, MissionEventsResult)
/// can flow across `async let` boundaries.
struct AnyCodable: Codable, Hashable, @unchecked Sendable {
    let value: Any

    init(_ value: Any) {
        self.value = value
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()

        if container.decodeNil() {
            value = NSNull()
        } else if let bool = try? container.decode(Bool.self) {
            value = bool
        } else if let int = try? container.decode(Int.self) {
            value = int
        } else if let double = try? container.decode(Double.self) {
            value = double
        } else if let string = try? container.decode(String.self) {
            value = string
        } else if let array = try? container.decode([AnyCodable].self) {
            value = array.map { $0.value }
        } else if let dict = try? container.decode([String: AnyCodable].self) {
            value = dict.mapValues { $0.value }
        } else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "Unable to decode value")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()

        switch value {
        case is NSNull:
            try container.encodeNil()
        case let bool as Bool:
            try container.encode(bool)
        case let int as Int:
            try container.encode(int)
        case let double as Double:
            try container.encode(double)
        case let string as String:
            try container.encode(string)
        case let array as [Any]:
            try container.encode(array.map { AnyCodable($0) })
        case let dict as [String: Any]:
            try container.encode(dict.mapValues { AnyCodable($0) })
        default:
            try container.encode(String(describing: value))
        }
    }

    func hash(into hasher: inout Hasher) {
        switch value {
        case let bool as Bool:
            hasher.combine(bool)
        case let int as Int:
            hasher.combine(int)
        case let double as Double:
            hasher.combine(double)
        case let string as String:
            hasher.combine(string)
        default:
            hasher.combine(String(describing: value))
        }
    }

    static func == (lhs: AnyCodable, rhs: AnyCodable) -> Bool {
        switch (lhs.value, rhs.value) {
        case (let lhsValue as Bool, let rhsValue as Bool):
            return lhsValue == rhsValue
        case (let lhsValue as Int, let rhsValue as Int):
            return lhsValue == rhsValue
        case (let lhsValue as Double, let rhsValue as Double):
            return lhsValue == rhsValue
        case (let lhsValue as String, let rhsValue as String):
            return lhsValue == rhsValue
        default:
            return String(describing: lhs.value) == String(describing: rhs.value)
        }
    }

    var stringValue: String {
        switch value {
        case is NSNull:
            return "-"
        case let bool as Bool:
            return bool ? "Yes" : "No"
        case let int as Int:
            return int.formatted()
        case let double as Double:
            return double.formatted(.number.precision(.fractionLength(0...2)))
        case let string as String:
            return string
        default:
            return String(describing: value)
        }
    }
}
