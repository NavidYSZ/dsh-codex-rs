import Foundation

// The UI ↔ server protocol (docs/rust/SPEC.md §6) as the app reads it. Loosely typed parts stay `JSONValue`.

/// `GET /api/info`.
struct ServerInfo: Decodable, Hashable, Sendable {
    var version: String
    var platform: String?
    var locale: String?
    var models: [ModelInfo]
    var settings: Settings
    var apiKeyConfigured: Bool
    /// SPEC §3.1: the server's identity (absent on builds before the Aether round).
    var server: PairingPayload.ServerIdentity?
    var presets: [Preset]?
    var automation: Automation?
    var ui: UIPrefs?

    struct Settings: Decodable, Hashable, Sendable {
        var model: String?
        var effort: String?
        var sandbox: String?
        var approval: String?
        var defaultPreset: String?
    }

    struct Preset: Decodable, Hashable, Sendable, Identifiable {
        var id: String
        var name: String
        var description: String?
    }

    struct Automation: Decodable, Hashable, Sendable {
        var goals: Bool?
        var schedules: Bool?
        var team: Bool?
    }

    struct UIPrefs: Decodable, Hashable, Sendable {
        var workDetails: String?
    }

    private enum CodingKeys: String, CodingKey { case version, platform, locale, models, settings, apiKeyConfigured, server, presets, automation, ui }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decodeIfPresent(String.self, forKey: .version) ?? ""
        platform = try c.decodeIfPresent(String.self, forKey: .platform)
        locale = try c.decodeIfPresent(String.self, forKey: .locale)
        models = try c.decodeIfPresent([ModelInfo].self, forKey: .models) ?? []
        settings = try c.decodeIfPresent(Settings.self, forKey: .settings) ?? Settings()
        apiKeyConfigured = try c.decodeIfPresent(Bool.self, forKey: .apiKeyConfigured) ?? false
        server = try? c.decodeIfPresent(PairingPayload.ServerIdentity.self, forKey: .server)
        presets = try? c.decodeIfPresent([Preset].self, forKey: .presets)
        automation = try? c.decodeIfPresent(Automation.self, forKey: .automation)
        ui = try? c.decodeIfPresent(UIPrefs.self, forKey: .ui)
    }

    init(version: String, models: [ModelInfo] = [], settings: Settings = Settings(), apiKeyConfigured: Bool = true,
         server: PairingPayload.ServerIdentity? = nil, presets: [Preset]? = nil) {
        self.version = version
        self.models = models
        self.settings = settings
        self.apiKeyConfigured = apiKeyConfigured
        self.server = server
        self.presets = presets
    }
}

struct ModelInfo: Decodable, Hashable, Sendable, Identifiable {
    var id: String
    var name: String
    var contextWindow: Int?
    var maxTokens: Int?
    var inputModalities: [String]?
    var reasoningEfforts: [String]?
    var defaultReasoningEffort: String?
    var provider: String?
    var providerName: String?
    var available: Bool?

    var acceptsImages: Bool { inputModalities?.contains("image") ?? false }
}

/// `GET /api/workspaces` rows.
struct Workspace: Decodable, Hashable, Sendable, Identifiable {
    var path: String
    var name: String
    var lastUsed: Double?
    var id: String { path }
}

/// `GET /api/sessions` rows.
struct SessionRow: Codable, Hashable, Sendable, Identifiable {
    var id: String
    var title: String = ""
    var cwd: String = ""
    var createdAt: Double?
    var updatedAt: Double?
    var status: String = "idle"
    var scratch = false
    var pinned = false
    var archived = false
    var unread = false
    var preset: String?
    var parentSession: String?

    private enum CodingKeys: String, CodingKey {
        case id, title, cwd, createdAt, updatedAt, status, scratch, pinned, archived, unread, preset, parentSession
    }

    init(id: String, title: String = "", cwd: String = "", createdAt: Double? = nil, updatedAt: Double? = nil, status: String = "idle",
         scratch: Bool = false, pinned: Bool = false, archived: Bool = false, unread: Bool = false, preset: String? = nil) {
        self.id = id
        self.title = title
        self.cwd = cwd
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.status = status
        self.scratch = scratch
        self.pinned = pinned
        self.archived = archived
        self.unread = unread
        self.preset = preset
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        cwd = try c.decodeIfPresent(String.self, forKey: .cwd) ?? ""
        createdAt = try c.decodeIfPresent(Double.self, forKey: .createdAt)
        updatedAt = try c.decodeIfPresent(Double.self, forKey: .updatedAt)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "idle"
        scratch = try c.decodeIfPresent(Bool.self, forKey: .scratch) ?? false
        pinned = try c.decodeIfPresent(Bool.self, forKey: .pinned) ?? false
        archived = try c.decodeIfPresent(Bool.self, forKey: .archived) ?? false
        unread = try c.decodeIfPresent(Bool.self, forKey: .unread) ?? false
        preset = try c.decodeIfPresent(String.self, forKey: .preset)
        parentSession = try c.decodeIfPresent(String.self, forKey: .parentSession)
    }

    /// Latest activity, for ordering.
    var when: Double { updatedAt ?? createdAt ?? 0 }
    var isBusy: Bool { status == "running" || status == "waiting-approval" }
}

/// A session log line (dsh format v4 envelope). Extra top-level keys (`surfaceOp`, …) are ignored.
struct SessionEvent: Codable, Hashable, Sendable {
    var type: String
    var seq: Int
    var time: Double
    var data: JSONValue

    private enum CodingKeys: String, CodingKey { case type, seq, time, data }

    init(type: String, seq: Int, time: Double, data: JSONValue = .object([:])) {
        self.type = type
        self.seq = seq
        self.time = time
        self.data = data
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = try c.decode(String.self, forKey: .type)
        seq = try c.decodeIfPresent(Int.self, forKey: .seq) ?? 0
        time = try c.decodeIfPresent(Double.self, forKey: .time) ?? 0
        data = try c.decodeIfPresent(JSONValue.self, forKey: .data) ?? .object([:])
    }
}

/// `GET /api/sessions/:id`.
struct SessionDetail: Decodable, Sendable {
    var header: JSONValue
    var events: [SessionEvent]
    var status: String

    private enum CodingKeys: String, CodingKey { case header, events, status }

    init(header: JSONValue, events: [SessionEvent], status: String) {
        self.header = header
        self.events = events
        self.status = status
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        header = try c.decodeIfPresent(JSONValue.self, forKey: .header) ?? .null
        events = try c.decodeIfPresent([SessionEvent].self, forKey: .events) ?? []
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "idle"
    }

    var cwd: String { header["cwd"]?.string ?? "" }
    var preset: String? { header["agentPreset"]?.string }
}

/// An uploaded image, as `POST /api/attachments` returned it; sent back verbatim in prompts.
struct ImageAttachmentRef: Codable, Hashable, Sendable {
    var raw: JSONValue

    init(raw: JSONValue) { self.raw = raw }
    init(from decoder: Decoder) throws { raw = try JSONValue(from: decoder) }
    func encode(to encoder: Encoder) throws { try raw.encode(to: encoder) }

    var attachmentId: String { raw["attachmentId"]?.string ?? "" }
    var mediaType: String { raw["mediaType"]?.string ?? "image/png" }
    var width: Int? { raw["width"]?.int }
    var height: Int? { raw["height"]?.int }
}

/// `GET /api/fs/dirs`.
struct FsDirs: Decodable, Hashable, Sendable {
    var path: String
    var parent: String?
    var root: String?
    var home: String?
    var roots: [String]?
    var git: Bool?
    var entries: [Entry]
    var truncated: Bool?

    struct Entry: Decodable, Hashable, Sendable, Identifiable {
        var name: String
        var path: String
        var git: Bool?
        var hidden: Bool?
        var id: String { path }
    }
}

/// `GET /api/fs/suggestions`.
struct FsSuggestions: Decodable, Hashable, Sendable {
    var repos: [Repo]

    struct Repo: Decodable, Hashable, Sendable, Identifiable {
        var name: String
        var path: String
        var id: String { path }
    }
}

/// `GET /api/sessions/:id/stats`: read dynamically, the UI only formats a few numbers.
struct SessionStats: Hashable, Sendable {
    var raw: JSONValue

    var contextPercent: Double? { raw["context"]?["percent"]?.double }
    var contextUsed: Int? { raw["context"]?["usedTokens"]?.int }
    var contextWindow: Int? { raw["context"]?["contextWindow"]?.int }
    var totalCostUsd: Double? { raw["totals"]?["costUsd"]?.double }

    struct TurnUsage: Hashable, Sendable {
        var turn: Int
        var tokens: Int
        var costUsd: Double
        var status: String?
    }

    var turns: [TurnUsage] {
        (raw["turns"]?.array ?? []).compactMap { t in
            guard let n = t["turn"]?.int else { return nil }
            let tokens = (t["inputTokens"]?.int ?? 0) + (t["cacheReadTokens"]?.int ?? 0) + (t["cacheWriteTokens"]?.int ?? 0) + (t["outputTokens"]?.int ?? 0)
            return TurnUsage(turn: n, tokens: tokens, costUsd: t["costUsd"]?.double ?? 0, status: t["status"]?.string)
        }
    }
}

// MARK: - WebSocket messages

struct DeltaMessage: Hashable, Sendable {
    var sessionId: String?
    var turn: Int?
    var step: Int?
    var index: Int
    var blockType: String
    var text: String
}

struct ApprovalMessage: Hashable, Sendable {
    var sessionId: String?
    var callId: String
    var tool: String
    var summary: String
    var justification: String
}

struct LiveMessage: Hashable, Sendable {
    var sessionId: String?
    var tool: String
    var args: JSONValue?
    var active: Bool
    var ended: Bool
    var activity: String?
    var place: String?
    var width: Double?
    var height: Double?
    var cursor: Cursor?
    /// Base64 JPEG of the latest frame.
    var image: String?
    var attachment: ImageAttachmentRef?
    var meta: JSONValue?
    var error: String?
    var time: Double

    struct Cursor: Hashable, Sendable {
        var x: Double
        var y: Double
        var click: Bool
    }
}

/// What `/api/stream` sends (docs/rust/SPEC.md §6, "WebSocket messages").
enum StreamMessage: Sendable {
    case event(sessionId: String?, SessionEvent)
    case delta(DeltaMessage)
    case status(sessionId: String?, status: String, error: String?)
    case approval(ApprovalMessage)
    case goal(sessionId: String?, JSONValue)
    case scheduleChanged
    case live(LiveMessage)
    case unknown(kind: String)

    init(json: JSONValue) {
        let kind = json["kind"]?.string ?? ""
        let sid = json["sessionId"]?.string
        switch kind {
        case "event":
            guard let ev = json["event"], let type = ev["type"]?.string else { self = .unknown(kind: kind); return }
            self = .event(sessionId: sid, SessionEvent(type: type, seq: ev["seq"]?.int ?? 0, time: ev["time"]?.double ?? 0, data: ev["data"] ?? .object([:])))
        case "delta":
            self = .delta(DeltaMessage(sessionId: sid, turn: json["turn"]?.int, step: json["step"]?.int, index: json["index"]?.int ?? 0,
                                       blockType: json["blockType"]?.string ?? "text", text: json["text"]?.string ?? ""))
        case "status":
            self = .status(sessionId: sid, status: json["status"]?.string ?? "idle", error: json["error"]?.string)
        case "approval":
            self = .approval(ApprovalMessage(sessionId: sid, callId: json["callId"]?.string ?? "", tool: json["tool"]?.string ?? "",
                                             summary: json["summary"]?.string ?? "", justification: json["justification"]?.string ?? ""))
        case "goal":
            self = .goal(sessionId: sid, json["goal"] ?? .null)
        case "schedule-changed":
            self = .scheduleChanged
        case "live":
            var cursor: LiveMessage.Cursor?
            if let c = json["cursor"], let x = c["x"]?.double, let y = c["y"]?.double {
                cursor = LiveMessage.Cursor(x: x, y: y, click: c["click"]?.bool ?? false)
            }
            self = .live(LiveMessage(
                sessionId: sid, tool: json["tool"]?.string ?? "browser", args: json["args"], active: json["active"]?.bool ?? false,
                ended: json["ended"]?.bool ?? false, activity: json["activity"]?.string, place: json["place"]?.string,
                width: json["width"]?.double, height: json["height"]?.double, cursor: cursor, image: json["image"]?.string,
                attachment: json["attachment"].flatMap { $0.isNull ? nil : ImageAttachmentRef(raw: $0) }, meta: json["meta"],
                error: json["error"]?.string, time: json["time"]?.double ?? Date().timeIntervalSince1970 * 1000))
        default:
            self = .unknown(kind: kind)
        }
    }
}

/// The body of `POST /api/sessions/:id/prompt`.
struct PromptBody: Encodable, Sendable {
    var text: String
    var images: [ImageAttachmentRef]?
    var model: String?
    var effort: String?
    var sandbox: String?
    var steer: Bool?
    var timeZone: String?
}

// MARK: - Errors

/// Errors the networking layer raises; `message` is always a sentence a person understands.
enum ApiError: Error, Hashable, Sendable {
    /// Nothing answered (DNS, refused, no route).
    case offline(String)
    case timeout
    case unauthorized
    case http(status: Int, message: String?, code: String?)
    case badResponse
    case cancelled
    case plainHTTPRefused(host: String)
    case plainHTTPOff
    case certificateMismatch
    case hostKeyMismatch
    case sshAuthFailed
    case sshConnect(host: String, String)
    case sshClosed
    case noToken
    case noTransport([String])
    case hubOffline
    case hubSignedOut
    case badKey
    case tooLarge
    case invalidURL

    var status: Int? {
        if case .http(let s, _, _) = self { return s }
        if case .unauthorized = self { return 401 }
        return nil
    }

    var message: String {
        switch self {
        case .offline: return L.errorOffline
        case .timeout: return L.errorTimeout
        case .unauthorized: return L.errorUnauthorized
        case .http(let status, let message, _):
            if let message, !message.isEmpty { return L.errorHttpMessage(status, message) }
            return L.errorHttp(status)
        case .badResponse: return L.errorBadResponse
        case .cancelled: return L.errorCancelled
        case .plainHTTPRefused(let host): return L.errorPlainHttpRefused(host)
        case .plainHTTPOff: return L.errorPlainHttpOff
        case .certificateMismatch: return L.errorCertMismatch
        case .hostKeyMismatch: return L.errorHostKeyMismatch
        case .sshAuthFailed: return L.errorSshAuth
        case .sshConnect(let host, _): return L.errorSshConnect(host)
        case .sshClosed: return L.errorSshClosed
        case .noToken: return L.errorNoToken
        case .noTransport(let reasons):
            let unique = reasons.filter { !$0.isEmpty }
            return unique.isEmpty ? L.errorNoTransport : L.errorNoTransport + " " + unique.joined(separator: " ")
        case .hubOffline: return L.errorHubOffline
        case .hubSignedOut: return L.errorHubSignedOut
        case .badKey: return L.errorBadKey
        case .tooLarge: return L.errorTooLarge
        case .invalidURL: return L.errorInvalidURL
        }
    }

    /// `true` when the device could not be reached at all (as opposed to answering with an error).
    var isUnreachable: Bool {
        switch self {
        case .offline, .timeout, .sshConnect, .sshClosed, .hubOffline, .noTransport: return true
        default: return false
        }
    }

    /// Any error as an `ApiError`, so every message shown is a sentence.
    static func wrap(_ error: Error) -> ApiError {
        if let e = error as? ApiError { return e }
        if error is CancellationError { return .cancelled }
        let ns = error as NSError
        if ns.domain == NSURLErrorDomain {
            switch ns.code {
            case NSURLErrorTimedOut: return .timeout
            case NSURLErrorCancelled: return .cancelled
            case NSURLErrorServerCertificateUntrusted, NSURLErrorServerCertificateHasBadDate, NSURLErrorServerCertificateHasUnknownRoot,
                NSURLErrorServerCertificateNotYetValid, NSURLErrorClientCertificateRejected:
                return .certificateMismatch
            // A handshake that broke off (a VPN interface answering for an address, a reset) is "not reachable",
            // not a wrong certificate: pin mismatches are reported by PinningDelegate.
            case NSURLErrorSecureConnectionFailed:
                return .offline(ns.localizedDescription)
            default: return .offline(ns.localizedDescription)
            }
        }
        return .offline(ns.localizedDescription)
    }
}

extension ApiError: LocalizedError {
    var errorDescription: String? { message }
}
