import Foundation

// The accounts hub (SPEC §5): sign up, sign in, the user's servers, and the relay through which a server is
// reached that is not on the same network. Implemented against the contract; the hub itself is not deployed yet.

struct HubUser: Codable, Hashable, Sendable {
    var id: String
    var email: String
    var name: String?
    var emailVerified: Bool?
    var createdAt: Double?
}

struct HubTier: Codable, Hashable, Sendable {
    var id: String
    var name: String
    var limits: Limits?
    var priceEurMonthly: Double?

    struct Limits: Codable, Hashable, Sendable {
        var servers: Int?
        var relay: Bool?
    }
}

struct HubServer: Codable, Hashable, Sendable, Identifiable {
    var id: String
    var name: String
    var hostname: String?
    var platform: String?
    var version: String?
    var online: Bool?
    var lastSeenAt: Double?
    var createdAt: Double?
}

struct HubMe: Codable, Hashable, Sendable {
    var user: HubUser
    var tier: HubTier?
    var usage: Usage?

    struct Usage: Codable, Hashable, Sendable {
        var servers: Int?
    }
}

struct HubSession: Codable, Hashable, Sendable {
    var hubURL: String
    var session: String
    var user: HubUser
}

final class HubClient: Sendable {
    let baseURL: URL
    private let session: URLSession

    init(baseURL: URL, session: URLSession? = nil) {
        self.baseURL = baseURL
        self.session = session ?? DirectTransport.makeSession(certSha256: nil, timeout: 20)
    }

    static func url(_ text: String) -> URL? {
        let s = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: s), let scheme = url.scheme?.lowercased(), url.host != nil, scheme == "https" || scheme == "http" else { return nil }
        if scheme == "http", !Loopback.isLoopback(url.host) { return nil }
        return url
    }

    private func request(_ method: String, _ path: String, bearer: String?, json: (any Encodable & Sendable)? = nil) async throws -> Data? {
        var base = baseURL.absoluteString
        if base.hasSuffix("/") { base.removeLast() }
        guard let url = URL(string: base + path) else { throw ApiError.invalidURL }
        var req = URLRequest(url: url)
        req.httpMethod = method
        if let bearer { req.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization") }
        if let json {
            req.httpBody = try JSONEncoder().encode(json)
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let data: Data
        let response: URLResponse
        do { (data, response) = try await session.data(for: req) } catch { throw ApiError.wrap(error) }
        guard let http = response as? HTTPURLResponse else { throw ApiError.badResponse }
        guard (200..<300).contains(http.statusCode) else {
            let j = try? JSONDecoder().decode(JSONValue.self, from: data)
            let code = j?["code"]?.string
            if http.statusCode == 401 { throw ApiError.http(status: 401, message: j?["error"]?.string, code: code) }
            if http.statusCode == 503, code == "server-offline" { throw ApiError.hubOffline }
            throw ApiError.http(status: http.statusCode, message: j?["error"]?.string, code: code)
        }
        if http.statusCode == 204 { return nil }
        return data
    }

    private func decode<T: Decodable>(_ data: Data?) throws -> T {
        guard let data else { throw ApiError.badResponse }
        do { return try JSONDecoder().decode(T.self, from: data) } catch { throw ApiError.badResponse }
    }

    struct AuthResponse: Decodable, Sendable {
        var session: String
        var user: HubUser
        var verification: String?
    }

    func signup(email: String, password: String, name: String?) async throws -> AuthResponse {
        var body: [String: JSONValue] = ["email": .string(email), "password": .string(password)]
        if let name, !name.isEmpty { body["name"] = .string(name) }
        return try decode(try await request("POST", "/v1/auth/signup", bearer: nil, json: JSONValue.object(body)))
    }

    func login(email: String, password: String) async throws -> AuthResponse {
        try decode(try await request("POST", "/v1/auth/login", bearer: nil, json: JSONValue.object(["email": .string(email), "password": .string(password)])))
    }

    func logout(session: String) async throws {
        _ = try await request("POST", "/v1/auth/logout", bearer: session)
    }

    func verify(session: String, code: String) async throws -> HubUser {
        struct R: Decodable { var user: HubUser }
        let r: R = try decode(try await request("POST", "/v1/auth/verify", bearer: session, json: JSONValue.object(["code": .string(code)])))
        return r.user
    }

    func me(session: String) async throws -> HubMe {
        try decode(try await request("GET", "/v1/me", bearer: session))
    }

    func servers(session: String) async throws -> [HubServer] {
        try decode(try await request("GET", "/v1/servers", bearer: session))
    }

    func tiers() async throws -> [HubTier] {
        try decode(try await request("GET", "/v1/tiers", bearer: nil))
    }

    func health() async throws -> Bool {
        let j: JSONValue = try decode(try await request("GET", "/v1/health", bearer: nil))
        return j["ok"]?.bool ?? false
    }
}

/// The `hub` transport: base URL `<hub>/v1/relay/<serverId>`, bearer = the hub session (SPEC §2, §5).
final class HubTransport: Transport {
    let kind: TransportKind = .hub
    private let hubURL: String
    private let serverId: String
    private let session: String?

    init(hubURL: String, serverId: String, session: String?) {
        self.hubURL = hubURL
        self.serverId = serverId
        self.session = session
    }

    func open() async throws -> Endpoint {
        guard let session, !session.isEmpty else { throw ApiError.hubSignedOut }
        guard let base = HubClient.url(hubURL) else { throw ApiError.invalidURL }
        var text = base.absoluteString
        if text.hasSuffix("/") { text.removeLast() }
        guard let relay = URL(string: text + "/v1/relay/\(serverId)") else { throw ApiError.invalidURL }
        let urlSession = DirectTransport.makeSession(certSha256: nil)
        // The relay answers 503 `server-offline` when the server is not connected; a probe tells now.
        var req = URLRequest(url: relay.appending(path: "api/info"))
        req.timeoutInterval = 10
        req.setValue("Bearer \(session)", forHTTPHeaderField: "Authorization")
        let (data, response): (Data, URLResponse)
        do { (data, response) = try await urlSession.data(for: req) } catch { throw ApiError.wrap(error) }
        guard let http = response as? HTTPURLResponse else { throw ApiError.badResponse }
        if http.statusCode == 401 { throw ApiError.hubSignedOut }
        if http.statusCode == 503 { throw ApiError.hubOffline }
        guard (200..<300).contains(http.statusCode) else {
            let j = try? JSONDecoder().decode(JSONValue.self, from: data)
            throw ApiError.http(status: http.statusCode, message: j?["error"]?.string, code: j?["code"]?.string)
        }
        return Endpoint(kind: .hub, baseURL: relay, session: urlSession, bearer: FixedBearer(token: session), label: base.host ?? hubURL)
    }

    func close() async {}
}
