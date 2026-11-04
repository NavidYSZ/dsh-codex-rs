import Foundation

/// A paired server as the app keeps it on disk: identity and the ways to reach it, **without secrets**.
/// The client token, the SSH private key and a token-command result live in the Keychain (`SecretStore`).
struct Device: Codable, Hashable, Sendable, Identifiable {
    /// The server's stable id (`server.json`, SPEC §3.3); pairing the same server again replaces the entry.
    var id: String
    var name: String
    var hostname: String?
    var platform: String?
    var version: String?
    var clientId: String?
    var transports: [StoredTransport]
    /// The transport that worked last time; tried first on the next connect.
    var lastTransport: TransportKind?
    var lastConnectedAt: Date?
    var addedAt: Date
    var isDemo = false

    enum Kind: String, Sendable { case laptop, server, cloud, demo }

    var kind: Kind {
        if isDemo { return .demo }
        if platform == "darwin" || platform == "macos" { return .laptop }
        if transports.allSatisfy({ if case .hub = $0 { return true } else { return false } }), !transports.isEmpty { return .cloud }
        return .server
    }

    /// The server's identity as the pairing payload carries it.
    var identity: PairingPayload.ServerIdentity {
        PairingPayload.ServerIdentity(id: id, name: name, hostname: hostname, platform: platform, version: version)
    }

    init(id: String, name: String, hostname: String? = nil, platform: String? = nil, version: String? = nil, clientId: String? = nil,
         transports: [StoredTransport], lastTransport: TransportKind? = nil, lastConnectedAt: Date? = nil, addedAt: Date = Date(), isDemo: Bool = false) {
        self.id = id
        self.name = name
        self.hostname = hostname
        self.platform = platform
        self.version = version
        self.clientId = clientId
        self.transports = transports
        self.lastTransport = lastTransport
        self.lastConnectedAt = lastConnectedAt
        self.addedAt = addedAt
        self.isDemo = isDemo
    }

    /// The device of a pairing payload (secrets left out; `SecretStore` keeps them).
    init(payload: PairingPayload, addedAt: Date = Date()) {
        self.init(
            id: payload.server.id, name: payload.server.name.isEmpty ? (payload.server.hostname ?? payload.server.id) : payload.server.name,
            hostname: payload.server.hostname, platform: payload.server.platform, version: payload.server.version,
            clientId: payload.client?.id, transports: payload.transports.compactMap(StoredTransport.init(transport:)), addedAt: addedAt)
    }

    /// The transports in the order to try: the one that worked last, then payload order.
    var orderedTransports: [StoredTransport] {
        guard let last = lastTransport, let i = transports.firstIndex(where: { $0.kind == last }) else { return transports }
        var out = transports
        let t = out.remove(at: i)
        out.insert(t, at: 0)
        return out
    }

    static let demoID = "demo"
}

/// A transport without its secret (`privateKey` of `ssh`); the token is per device, not per transport.
enum StoredTransport: Codable, Hashable, Sendable {
    case https(urls: [String], certSha256: String?)
    case http(urls: [String])
    case ssh(SSHOptions)
    case hub(url: String, serverId: String)
    case demo

    struct SSHOptions: Codable, Hashable, Sendable {
        var host: String
        var port: Int
        var user: String
        var hostKeySha256: String
        var remoteHost: String
        var remotePort: Int
        var tokenCommand: String?
    }

    init?(transport: PairingTransport) {
        switch transport {
        case .https(let v): self = .https(urls: v.urls, certSha256: v.certSha256)
        case .http(let v): self = .http(urls: v.urls)
        case .ssh(let v):
            self = .ssh(SSHOptions(host: v.host, port: v.port, user: v.user, hostKeySha256: v.hostKeySha256,
                                   remoteHost: v.remoteHost, remotePort: v.remotePort, tokenCommand: v.tokenCommand))
        case .hub(let v): self = .hub(url: v.url, serverId: v.serverId)
        case .unknown: return nil
        }
    }

    var kind: TransportKind {
        switch self {
        case .https: return .https
        case .http: return .http
        case .ssh: return .ssh
        case .hub: return .hub
        case .demo: return .demo
        }
    }

    /// A short line for the device card ("192.168.178.20:7443", "ubuntu@130.162.241.44").
    var summary: String {
        switch self {
        case .https(let urls, _), .http(let urls):
            return urls.first.map { $0.replacingOccurrences(of: #"^https?://"#, with: "", options: .regularExpression) } ?? ""
        case .ssh(let o): return "\(o.user)@\(o.host)" + (o.port == 22 ? "" : ":\(o.port)")
        case .hub(let url, _): return url.replacingOccurrences(of: #"^https?://"#, with: "", options: .regularExpression)
        case .demo: return L.demoHint
        }
    }
}

extension TransportKind {
    var label: String {
        switch self {
        case .https: return L.devicesTransportHttps
        case .http: return L.devicesTransportHttp
        case .ssh: return L.devicesTransportSsh
        case .hub: return L.devicesTransportHub
        case .demo: return L.devicesTransportDemo
        case .unknown: return "?"
        }
    }
}
