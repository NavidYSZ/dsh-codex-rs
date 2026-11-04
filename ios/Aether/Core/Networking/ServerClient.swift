import Foundation

/// What the stream delivers besides messages: its own state.
enum StreamEvent: Sendable {
    case open
    case retry
    case message(StreamMessage)
}

/// The server as the app uses it (docs/rust/SPEC.md §6, the subset the phone needs). `ServerClient` talks to a
/// real dsh-codex; `FixtureClient` replays the web UI's fixtures for the demo.
protocol ServerAPI: AnyObject, Sendable {
    var kind: TransportKind { get }
    var label: String { get }
    func info() async throws -> ServerInfo
    func workspaces() async throws -> [Workspace]
    func fsDirs(path: String, hidden: Bool) async throws -> FsDirs
    func fsSuggestions() async throws -> FsSuggestions
    func sessions() async throws -> [SessionRow]
    /// An empty `cwd` starts a chat without a project; `preset` is the chat's mode (default: the server's).
    func createSession(cwd: String?, preset: String?) async throws -> String
    func session(_ id: String) async throws -> SessionDetail
    /// `{title?, pinned?, archived?, seen?, unread?}` → the session's list row.
    func updateSession(_ id: String, patch: JSONValue) async throws -> SessionRow?
    func deleteSession(_ id: String) async throws
    func prompt(_ id: String, body: PromptBody) async throws
    func cancel(_ id: String) async throws
    func decide(callId: String, decision: String) async throws
    func upload(data: Data, mediaType: String) async throws -> ImageAttachmentRef
    func image(_ ref: ImageAttachmentRef) async throws -> Data
    func stats(_ id: String) async throws -> SessionStats
    /// Live feed of one session (or, without an id, status and approvals of all chats). Reconnects with backoff
    /// and resumes after the last event seq it delivered.
    func stream(sessionId: String?, after: Int) -> AsyncStream<StreamEvent>
}

/// REST + WebSocket on top of an `Endpoint` (ui/src/api.js `HttpApi`).
final class ServerClient: ServerAPI {
    let endpoint: Endpoint
    var kind: TransportKind { endpoint.kind }
    var label: String { endpoint.label }

    init(endpoint: Endpoint) {
        self.endpoint = endpoint
    }

    // MARK: Requests

    private func url(_ path: String) -> URL {
        var base = endpoint.baseURL.absoluteString
        if base.hasSuffix("/") { base.removeLast() }
        return URL(string: base + path) ?? endpoint.baseURL
    }

    private static func quote(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlQueryValueAllowed) ?? s
    }

    @discardableResult
    private func request(_ method: String, _ path: String, json: (any Encodable & Sendable)? = nil, raw: (Data, String)? = nil,
                         retried: Bool = false) async throws -> Data? {
        var req = URLRequest(url: url(path))
        req.httpMethod = method
        if let token = try await endpoint.bearer.current() { req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let raw {
            req.httpBody = raw.0
            req.setValue(raw.1, forHTTPHeaderField: "Content-Type")
        } else if let json {
            let encoder = JSONEncoder()
            req.httpBody = try encoder.encode(json)
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await endpoint.session.data(for: req)
        } catch {
            throw ApiError.wrap(error)
        }
        guard let http = response as? HTTPURLResponse else { throw ApiError.badResponse }
        if http.statusCode == 401 || http.statusCode == 403 {
            // A token from `tokenCommand` may have rotated: fetch it again once.
            if !retried, (try? await endpoint.bearer.refresh()) != nil {
                return try await request(method, path, json: json, raw: raw, retried: true)
            }
            throw ApiError.unauthorized
        }
        guard (200..<300).contains(http.statusCode) else {
            var message: String?
            var code: String?
            if let j = try? JSONDecoder().decode(JSONValue.self, from: data) {
                message = j["error"]?.string ?? j["error"]?["message"]?.string ?? j["message"]?.string
                code = j["code"]?.string
            }
            throw ApiError.http(status: http.statusCode, message: message, code: code)
        }
        if http.statusCode == 202 || http.statusCode == 204 { return nil }
        return data
    }

    private func get<T: Decodable>(_ path: String, as type: T.Type = T.self) async throws -> T {
        guard let data = try await request("GET", path) else { throw ApiError.badResponse }
        do { return try JSONDecoder().decode(T.self, from: data) } catch { throw ApiError.badResponse }
    }

    private func send<T: Decodable>(_ method: String, _ path: String, json: (any Encodable & Sendable)?, as type: T.Type = T.self) async throws -> T {
        guard let data = try await request(method, path, json: json) else { throw ApiError.badResponse }
        do { return try JSONDecoder().decode(T.self, from: data) } catch { throw ApiError.badResponse }
    }

    // MARK: ServerAPI

    func info() async throws -> ServerInfo { try await get("/api/info") }
    func workspaces() async throws -> [Workspace] { try await get("/api/workspaces") }

    func fsDirs(path: String, hidden: Bool) async throws -> FsDirs {
        try await get("/api/fs/dirs?path=\(Self.quote(path))\(hidden ? "&hidden=1" : "")")
    }

    func fsSuggestions() async throws -> FsSuggestions { try await get("/api/fs/suggestions") }
    func sessions() async throws -> [SessionRow] { try await get("/api/sessions") }

    func createSession(cwd: String?, preset: String?) async throws -> String {
        var body: [String: JSONValue] = [:]
        if let cwd, !cwd.isEmpty { body["cwd"] = .string(cwd) }
        if let preset, !preset.isEmpty { body["preset"] = .string(preset) }
        let out: JSONValue = try await send("POST", "/api/sessions", json: JSONValue.object(body))
        guard let id = out["id"]?.string else { throw ApiError.badResponse }
        return id
    }

    func session(_ id: String) async throws -> SessionDetail { try await get("/api/sessions/\(Self.quote(id))") }

    func updateSession(_ id: String, patch: JSONValue) async throws -> SessionRow? {
        guard let data = try await request("PATCH", "/api/sessions/\(Self.quote(id))", json: patch) else { return nil }
        return try? JSONDecoder().decode(SessionRow.self, from: data)
    }

    func deleteSession(_ id: String) async throws { try await request("DELETE", "/api/sessions/\(Self.quote(id))") }
    func prompt(_ id: String, body: PromptBody) async throws { try await request("POST", "/api/sessions/\(Self.quote(id))/prompt", json: body) }
    func cancel(_ id: String) async throws { try await request("POST", "/api/sessions/\(Self.quote(id))/cancel") }

    func decide(callId: String, decision: String) async throws {
        try await request("POST", "/api/approvals/\(Self.quote(callId))", json: JSONValue.object(["decision": .string(decision)]))
    }

    func upload(data: Data, mediaType: String) async throws -> ImageAttachmentRef {
        guard data.count <= 20 * 1024 * 1024 else { throw ApiError.tooLarge }
        guard let out = try await request("POST", "/api/attachments", raw: (data, mediaType)) else { throw ApiError.badResponse }
        do { return try JSONDecoder().decode(ImageAttachmentRef.self, from: out) } catch { throw ApiError.badResponse }
    }

    func image(_ ref: ImageAttachmentRef) async throws -> Data {
        guard !ref.attachmentId.isEmpty, let data = try await request("GET", "/api/attachments/\(Self.quote(ref.attachmentId))") else {
            throw ApiError.badResponse
        }
        return data
    }

    func stats(_ id: String) async throws -> SessionStats {
        SessionStats(raw: try await get("/api/sessions/\(Self.quote(id))/stats"))
    }

    // MARK: Stream

    func stream(sessionId: String?, after: Int) -> AsyncStream<StreamEvent> {
        let endpoint = endpoint
        let base = url("/api/stream")
        return AsyncStream { continuation in
            let task = Task {
                var last = after
                var tries = 0
                while !Task.isCancelled {
                    var comps = URLComponents(url: base, resolvingAgainstBaseURL: false)!
                    comps.scheme = comps.scheme == "https" ? "wss" : "ws"
                    var items: [URLQueryItem] = []
                    if let sessionId { items += [URLQueryItem(name: "session", value: sessionId), URLQueryItem(name: "after", value: String(last))] }
                    if let token = try? await endpoint.bearer.current() { items.append(URLQueryItem(name: "token", value: token)) }
                    comps.queryItems = items.isEmpty ? nil : items
                    guard let wsURL = comps.url else { break }
                    let socket = endpoint.session.webSocketTask(with: wsURL)
                    socket.maximumMessageSize = 64 * 1024 * 1024
                    socket.resume()
                    var opened = false
                    do {
                        while !Task.isCancelled {
                            let message = try await socket.receive()
                            if !opened {
                                opened = true
                                tries = 0
                                continuation.yield(.open)
                            }
                            let text: String
                            switch message {
                            case .string(let s): text = s
                            case .data(let d): text = String(decoding: d, as: UTF8.self)
                            @unknown default: continue
                            }
                            guard let json = JSONValue.parse(text) else { continue }
                            let msg = StreamMessage(json: json)
                            if case .event(_, let ev) = msg { last = max(last, ev.seq) }
                            continuation.yield(.message(msg))
                        }
                    } catch {
                        // fall through to the retry
                    }
                    socket.cancel(with: .goingAway, reason: nil)
                    if Task.isCancelled { break }
                    continuation.yield(.retry)
                    let delay = min(8000, 400 * (1 << min(tries, 6)))
                    tries += 1
                    try? await Task.sleep(for: .milliseconds(delay))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}

extension CharacterSet {
    /// What may stay unescaped in a query value (RFC 3986 unreserved, nothing else).
    static let urlQueryValueAllowed: CharacterSet = {
        var set = CharacterSet.alphanumerics
        set.insert(charactersIn: "-._~")
        return set
    }()
}
