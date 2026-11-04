import Foundation

/// Dynamic JSON, for the loosely typed parts of the protocol: session event `data`, tool arguments,
/// result `meta`. Decodes anything, re-encodes losslessly (numbers as Double).
enum JSONValue: Hashable, Sendable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case null
    case array([JSONValue])
    case object([String: JSONValue])

    // MARK: Accessors

    var string: String? { if case .string(let s) = self { return s } else { return nil } }
    var double: Double? { if case .number(let d) = self { return d } else { return nil } }
    var int: Int? {
        guard case .number(let d) = self, d.isFinite, abs(d) < 9.0e15 else { return nil }
        return Int(d)
    }
    var bool: Bool? { if case .bool(let b) = self { return b } else { return nil } }
    var array: [JSONValue]? { if case .array(let a) = self { return a } else { return nil } }
    var object: [String: JSONValue]? { if case .object(let o) = self { return o } else { return nil } }
    var isNull: Bool { if case .null = self { return true } else { return false } }

    subscript(key: String) -> JSONValue? {
        guard case .object(let o) = self else { return nil }
        return o[key]
    }

    subscript(index: Int) -> JSONValue? {
        guard case .array(let a) = self, a.indices.contains(index) else { return nil }
        return a[index]
    }

    /// `true` for JSON `true`, `false` for `false`, `null` and a missing value; like JavaScript's `!!`.
    var truthy: Bool {
        switch self {
        case .bool(let b): return b
        case .null: return false
        case .number(let d): return d != 0
        case .string(let s): return !s.isEmpty
        case .array, .object: return true
        }
    }

    /// The JavaScript reading of `typeof x === 'number'` for coordinates and counts.
    var isNumber: Bool { if case .number = self { return true } else { return false } }

    // MARK: Conversion

    init(any: Any) {
        switch any {
        case let v as JSONValue: self = v
        case is NSNull: self = .null
        case let s as String: self = .string(s)
        case let b as Bool where type(of: any) == type(of: NSNumber(value: true)) && CFGetTypeID(any as CFTypeRef) == CFBooleanGetTypeID():
            self = .bool(b)
        case let n as NSNumber: self = .number(n.doubleValue)
        case let d as Double: self = .number(d)
        case let i as Int: self = .number(Double(i))
        case let a as [Any]: self = .array(a.map(JSONValue.init(any:)))
        case let o as [String: Any]: self = .object(o.mapValues(JSONValue.init(any:)))
        default: self = .null
        }
    }

    var anyValue: Any {
        switch self {
        case .string(let s): return s
        case .number(let d): return d == d.rounded() && abs(d) < 9.0e15 ? Int(d) as Any : d
        case .bool(let b): return b
        case .null: return NSNull()
        case .array(let a): return a.map(\.anyValue)
        case .object(let o): return o.mapValues(\.anyValue)
        }
    }

    /// Parses JSON text; nil when it is not JSON.
    static func parse(_ text: String) -> JSONValue? {
        guard let data = text.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(JSONValue.self, from: data)
    }

    /// Compact JSON text (keys sorted, so output is stable).
    func encoded(pretty: Bool = false) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = pretty ? [.sortedKeys, .prettyPrinted] : [.sortedKeys]
        guard let data = try? encoder.encode(self) else { return "" }
        return String(decoding: data, as: UTF8.self)
    }
}

extension JSONValue: Codable {
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null; return }
        if let b = try? c.decode(Bool.self) { self = .bool(b); return }
        if let d = try? c.decode(Double.self) { self = .number(d); return }
        if let s = try? c.decode(String.self) { self = .string(s); return }
        if let a = try? c.decode([JSONValue].self) { self = .array(a); return }
        if let o = try? c.decode([String: JSONValue].self) { self = .object(o); return }
        throw DecodingError.dataCorruptedError(in: c, debugDescription: "not JSON")
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let d):
            if d == d.rounded(), abs(d) < 9.0e15 { try c.encode(Int64(d)) } else { try c.encode(d) }
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }
}

extension JSONValue: ExpressibleByStringLiteral, ExpressibleByIntegerLiteral, ExpressibleByBooleanLiteral,
    ExpressibleByFloatLiteral, ExpressibleByArrayLiteral, ExpressibleByDictionaryLiteral, ExpressibleByNilLiteral {
    init(stringLiteral value: String) { self = .string(value) }
    init(integerLiteral value: Int) { self = .number(Double(value)) }
    init(booleanLiteral value: Bool) { self = .bool(value) }
    init(floatLiteral value: Double) { self = .number(value) }
    init(arrayLiteral elements: JSONValue...) { self = .array(elements) }
    init(dictionaryLiteral elements: (String, JSONValue)...) { self = .object(Dictionary(elements, uniquingKeysWith: { $1 })) }
    init(nilLiteral: ()) { self = .null }
}
