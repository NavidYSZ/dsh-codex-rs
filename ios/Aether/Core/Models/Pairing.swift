import Foundation

/// A pairing payload (SPEC §2.1): what a QR code, a link or a pasted text carries so the phone can reach a server.
struct PairingPayload: Codable, Hashable, Sendable {
    var v: Int = 1
    var server: ServerIdentity
    var client: ClientIdentity?
    /// The client token the server issued for this phone; absent when only `tokenCommand` is available.
    var token: String?
    var transports: [PairingTransport]

    struct ServerIdentity: Codable, Hashable, Sendable {
        var id: String
        var name: String
        var hostname: String?
        var platform: String?
        var version: String?
    }

    struct ClientIdentity: Codable, Hashable, Sendable {
        var id: String
        var name: String?
    }
}

/// One way to reach the server; unknown kinds are kept as `.unknown` and skipped.
enum PairingTransport: Hashable, Sendable {
    case https(HTTPS)
    case http(HTTP)
    case ssh(SSH)
    case hub(Hub)
    case unknown(kind: String)

    struct HTTPS: Codable, Hashable, Sendable {
        var urls: [String]
        /// Lowercase hex SHA-256 of the DER certificate; absent = system validation (Tailscale, a real certificate).
        var certSha256: String?
    }

    struct HTTP: Codable, Hashable, Sendable {
        var urls: [String]
    }

    struct SSH: Codable, Hashable, Sendable {
        var host: String
        var port: Int = 22
        var user: String
        /// `ssh-keygen -l` form: `SHA256:` + base64 without padding.
        var hostKeySha256: String
        /// OpenSSH PEM of an ed25519 key made for this phone.
        var privateKey: String
        var remoteHost: String = "127.0.0.1"
        var remotePort: Int
        var tokenCommand: String?
    }

    struct Hub: Codable, Hashable, Sendable {
        var url: String
        var serverId: String
    }

    var kind: TransportKind {
        switch self {
        case .https: return .https
        case .http: return .http
        case .ssh: return .ssh
        case .hub: return .hub
        case .unknown: return .unknown
        }
    }
}

enum TransportKind: String, Codable, Hashable, Sendable, CaseIterable {
    case https, http, ssh, hub, demo, unknown
}

extension PairingTransport: Codable {
    private enum CodingKeys: String, CodingKey { case kind }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        let single = try decoder.singleValueContainer()
        switch kind {
        case "https": self = .https(try single.decode(HTTPS.self))
        case "http": self = .http(try single.decode(HTTP.self))
        case "ssh": self = .ssh(try single.decode(SSH.self))
        case "hub": self = .hub(try single.decode(Hub.self))
        default: self = .unknown(kind: kind)
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(kind.rawValue, forKey: .kind)
        switch self {
        case .https(let v): try v.encode(to: encoder)
        case .http(let v): try v.encode(to: encoder)
        case .ssh(let v): try v.encode(to: encoder)
        case .hub(let v): try v.encode(to: encoder)
        case .unknown: break
        }
    }
}

enum PairingError: Error, Equatable, Sendable {
    /// The text is not an Aether link, not base64 and not JSON.
    case notALink
    /// It decoded, but the JSON is not a pairing payload (missing server id, no transports, …).
    case invalidPayload(String)
}

extension PairingPayload {
    /// Parses `aether://pair#<base64url>`, the bare base64url text (standard base64 and padding tolerated) or the bare JSON.
    static func parse(_ raw: String) throws -> PairingPayload {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw PairingError.notALink }
        if text.hasPrefix("{") {
            return try decode(json: Data(text.utf8))
        }
        var body = text
        if let range = text.range(of: "aether://pair", options: [.caseInsensitive, .anchored]) {
            body = String(text[range.upperBound...])
            // `#` (per spec), or `?` / `/` from a copy that lost the fragment marker.
            while let first = body.first, "#?/".contains(first) { body.removeFirst() }
            // A `payload=` or `p=` query form, should a tool ever produce one.
            if let eq = body.firstIndex(of: "="), body[..<eq].allSatisfy({ $0.isLetter }) { body = String(body[body.index(after: eq)...]) }
        } else if text.lowercased().hasPrefix("aether:") {
            throw PairingError.notALink
        }
        body = body.removingPercentEncoding ?? body
        guard let data = Data(base64URL: body) else { throw PairingError.notALink }
        return try decode(json: data)
    }

    private static func decode(json: Data) throws -> PairingPayload {
        let payload: PairingPayload
        do {
            payload = try JSONDecoder().decode(PairingPayload.self, from: json)
        } catch {
            throw PairingError.invalidPayload(String(describing: error))
        }
        guard !payload.server.id.isEmpty else { throw PairingError.invalidPayload("server.id is empty") }
        guard payload.transports.contains(where: { $0.kind != .unknown }) else {
            throw PairingError.invalidPayload("no usable transport")
        }
        return payload
    }

    /// The `aether://pair#<base64url>` link of this payload (no padding).
    func link() throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(self)
        return "aether://pair#" + data.base64URLEncodedString()
    }
}

extension Data {
    /// base64url (RFC 4648 §5) without padding, tolerant of standard base64, padding and whitespace.
    init?(base64URL text: String) {
        var s = text.filter { !$0.isWhitespace }
        s = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        s = s.trimmingCharacters(in: CharacterSet(charactersIn: "="))
        let pad = (4 - s.count % 4) % 4
        if pad == 3 { return nil }
        s += String(repeating: "=", count: pad)
        guard let data = Data(base64Encoded: s) else { return nil }
        self = data
    }

    func base64URLEncodedString() -> String {
        base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .trimmingCharacters(in: CharacterSet(charactersIn: "="))
    }
}
