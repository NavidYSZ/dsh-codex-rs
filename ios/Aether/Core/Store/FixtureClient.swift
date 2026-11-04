import Foundation

/// Replays the web UI's fixtures (ui/fixtures/*.json, bundled) for "Demo ansehen": events before `live` are
/// history, the rest stream in with text and reasoning deltas, as dshx-server would send them (ui/src/api.js `FixtureApi`).
actor FixtureClient: ServerAPI {
    nonisolated let kind: TransportKind = .demo
    nonisolated let label: String = "Demo"

    private struct Fixture {
        var name: String
        var header: JSONValue
        var history: [SessionEvent]
        var pendingLive: [SessionEvent]
        var approval: JSONValue?
        var onApproval: JSONValue?
        var stats: JSONValue?
        var status: String
        var seq: Int
        var started = false
        var cancelled = false
    }

    private var fixtures: [String: Fixture] = [:]
    private var rows: [SessionRow] = []
    private var workspaces: [Workspace] = []
    private var subscribers: [UUID: (sessionId: String?, continuation: AsyncStream<StreamEvent>.Continuation)] = [:]
    private var created: SessionRow?
    private let speed: Double

    /// One demo server for the whole app, loaded once (concurrent callers wait for the same load).
    @MainActor private static var instance: Task<FixtureClient, Never>?

    @MainActor static func shared() async -> FixtureClient {
        if let instance { return await instance.value }
        let task = Task<FixtureClient, Never> {
            let c = FixtureClient()
            await c.load()
            return c
        }
        instance = task
        return await task.value
    }

    init(speed: Double = 1) {
        self.speed = speed
    }

    private func read(_ name: String) -> JSONValue? {
        guard let url = Bundle.main.url(forResource: name, withExtension: "json"), let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(JSONValue.self, from: data)
    }

    private func load() {
        let now = Date().timeIntervalSince1970 * 1000
        let shared = read("_shared") ?? .object([:])
        workspaces = (shared["workspaces"]?.array ?? []).compactMap { w in
            guard let path = w["path"]?.string else { return nil }
            return Workspace(path: path, name: w["name"]?.string ?? path, lastUsed: w["lastUsedAgo"]?.double.map { now - $0 })
        }
        for name in ["coding", "approval"] {
            guard let fx = read(name), let session = fx["session"], let header = session["header"] else { continue }
            let all = (session["events"]?.array ?? []).compactMap { e -> SessionEvent? in
                guard let type = e["type"]?.string else { return nil }
                return SessionEvent(type: type, seq: e["seq"]?.int ?? 0, time: e["time"]?.double ?? 0, data: e["data"] ?? .object([:]))
            }
            let live = fx["live"]?.int ?? all.count
            var history = Array(all.prefix(live))
            let pending = Array(all.dropFirst(live))
            // A replayed turn happens now: move its history next to the clock so durations stay sane.
            if !pending.isEmpty, let last = history.last {
                let shift = now - 1500 - last.time
                history = history.map { var e = $0; e.time += shift; return e }
            }
            let id = header["id"]?.string ?? name
            fixtures[id] = Fixture(name: name, header: header, history: history, pendingLive: pending, approval: fx["approval"].flatMap { $0.isNull ? nil : $0 },
                                   onApproval: fx["onApproval"], stats: fx["stats"], status: pending.isEmpty ? (fx["status"]?.string ?? "idle") : "running",
                                   seq: history.last?.seq ?? -1)
        }
        rows = (shared["sessions"]?.array ?? []).compactMap { s in
            guard let id = s["id"]?.string, fixtures[id] != nil else { return nil }
            return SessionRow(id: id, title: s["title"]?.string ?? "", cwd: s["cwd"]?.string ?? "", createdAt: s["createdAt"]?.double,
                              updatedAt: s["updatedAgo"]?.double.map { now - $0 }, status: s["status"]?.string ?? "idle",
                              unread: s["unread"]?.bool ?? false, preset: s["preset"]?.string)
        }
    }

    // MARK: ServerAPI

    func info() async throws -> ServerInfo {
        let models = [
            ModelInfo(id: "deepseek-flash", name: "DeepSeek-V4.1-Flash", contextWindow: 1_000_000, inputModalities: ["text", "image"],
                      reasoningEfforts: ["off", "low", "high", "max"], defaultReasoningEffort: "high", provider: "deepseek-official", providerName: "DeepSeek", available: true),
            ModelInfo(id: "deepseek-v4-pro", name: "DeepSeek-V4-Pro", contextWindow: 1_000_000, inputModalities: ["text"],
                      reasoningEfforts: ["off", "low", "high", "max"], defaultReasoningEffort: "high", provider: "deepseek-official", providerName: "DeepSeek", available: true),
        ]
        return ServerInfo(version: "0.3.0-dev", models: models,
                          settings: .init(model: "deepseek-flash", effort: "high", sandbox: "workspace-write", approval: "ask", defaultPreset: "standard"),
                          apiKeyConfigured: true, server: .init(id: Device.demoID, name: "Demo", hostname: "demo", platform: "darwin", version: "0.3.0-dev"),
                          presets: [.init(id: "standard", name: "Standard"), .init(id: "minimal", name: "Minimal")])
    }

    func workspaces() async throws -> [Workspace] { workspaces }

    func fsDirs(path: String, hidden: Bool) async throws -> FsDirs {
        let home = "/Users/alex"
        let p = path.isEmpty ? home : path
        let entries = p == home ? [FsDirs.Entry(name: "code", path: "\(home)/code", git: false, hidden: false)]
            : p == "\(home)/code" ? workspaces.filter { $0.path.hasPrefix("\(home)/code/") }.map { FsDirs.Entry(name: $0.name, path: $0.path, git: true, hidden: false) }
            : []
        return FsDirs(path: p, parent: p == home ? nil : (p as NSString).deletingLastPathComponent, root: "/", home: home, roots: [home], git: p.hasPrefix("\(home)/code/"), entries: entries, truncated: false)
    }

    func fsSuggestions() async throws -> FsSuggestions {
        FsSuggestions(repos: workspaces.filter { $0.path.hasPrefix("/Users/alex/code/") }.map { .init(name: $0.name, path: $0.path) })
    }

    func sessions() async throws -> [SessionRow] {
        var list = rows
        for i in list.indices { if let f = fixtures[list[i].id], f.status == "running" { list[i].status = "running" } }
        if let created { list.insert(created, at: 0) }
        return list
    }

    func createSession(cwd: String?, preset: String?) async throws -> String {
        let id = "session-demo-\(String(Int(Date().timeIntervalSince1970), radix: 36))"
        let folder = cwd?.isEmpty == false ? cwd! : "/Users/alex/.dsh-codex/chats/chat-\(id.suffix(8))"
        let header: JSONValue = .object(["type": "session", "version": 4, "id": .string(id), "createdAt": .number(Date().timeIntervalSince1970 * 1000),
                                         "cwd": .string(folder), "isSeeded": false, "delegationDepth": 0, "agentPreset": .string(preset ?? "standard")])
        fixtures[id] = Fixture(name: "new", header: header, history: [], pendingLive: [], approval: nil, onApproval: nil, stats: nil, status: "idle", seq: -1)
        let now = Date().timeIntervalSince1970 * 1000
        created = SessionRow(id: id, title: "", cwd: folder, createdAt: now, updatedAt: now, status: "idle", scratch: cwd?.isEmpty ?? true, preset: preset ?? "standard")
        return id
    }

    func session(_ id: String) async throws -> SessionDetail {
        guard let f = fixtures[id] else { throw ApiError.http(status: 404, message: "session not found", code: nil) }
        return SessionDetail(header: f.header, events: f.history, status: f.status)
    }

    func updateSession(_ id: String, patch: JSONValue) async throws -> SessionRow? {
        func apply(_ row: inout SessionRow) {
            if let t = patch["title"]?.string, !t.isEmpty { row.title = t }
            if let p = patch["pinned"]?.bool { row.pinned = p }
            if let a = patch["archived"]?.bool { row.archived = a }
            if patch["seen"]?.bool == true { row.unread = false }
            if let u = patch["unread"]?.bool { row.unread = u }
        }
        if let i = rows.firstIndex(where: { $0.id == id }) { apply(&rows[i]); return rows[i] }
        if created?.id == id { apply(&created!); return created }
        throw ApiError.http(status: 404, message: "session not found", code: nil)
    }

    func deleteSession(_ id: String) async throws {
        rows.removeAll { $0.id == id }
        if created?.id == id { created = nil }
    }

    func prompt(_ id: String, body: PromptBody) async throws {
        guard var f = fixtures[id] else { throw ApiError.http(status: 404, message: "session not found", code: nil) }
        f.cancelled = false
        let turn = 1 + f.history.filter { $0.type == "turn/start" }.count
        let msgId = "demo-\(String(Int(Date().timeIntervalSince1970 * 1000), radix: 36))"
        var content: [JSONValue] = [.object(["type": "text", "text": .string(body.text)])]
        for img in body.images ?? [] { content.append(.object(["type": "image", "attachment": img.raw])) }
        let titled = created?.id == id && (created?.title.isEmpty ?? false)
        var events: [SessionEvent] = [
            SessionEvent(type: "turn/start", seq: 0, time: 0, data: .object(["turn": .number(Double(turn))])),
            SessionEvent(type: "step/start", seq: 0, time: 0, data: .object(["turn": .number(Double(turn)), "step": 1])),
            SessionEvent(type: "user/message", seq: 0, time: 0, data: .object(["role": "user", "id": .string(msgId), "source": .object(["kind": "user"]), "content": .array(content)])),
        ]
        if titled {
            events.append(SessionEvent(type: "session/title", seq: 0, time: 0, data: .object(["title": .string(String(body.text.prefix(40))), "messageSeqs": [], "source": .object(["kind": "fallback"])])))
        }
        events += [
            SessionEvent(type: "assistant/message", seq: 0, time: 0, data: .object([
                "turn": .number(Double(turn)), "step": 1,
                "message": .object(["role": "assistant", "id": .string(msgId + "-a"),
                                    "content": .array([.object(["type": "reasoning", "text": "Fixture mode, so there is nothing to do but answer."]),
                                                       .object(["type": "text", "text": .string(L.demoReply)])]),
                                    "source": .object(["kind": "model", "provider": "deepseek-official", "model": .string(body.model ?? "deepseek-flash")])]),
                "stream": [], "usage": .object(["inputTokens": 900, "outputTokens": 40, "cacheReadTokens": 3200, "totalTokens": 4140]),
            ])),
            SessionEvent(type: "step/end", seq: 0, time: 0, data: .object(["turn": .number(Double(turn)), "step": 1])),
            SessionEvent(type: "turn/end", seq: 0, time: 0, data: .object(["turn": .number(Double(turn)), "reason": .object(["kind": "completed"])])),
        ]
        fixtures[id] = f
        setStatus(id, "running")
        let head = Array(events.prefix(titled ? 4 : 3))
        let tail = Array(events.dropFirst(titled ? 4 : 3))
        for ev in head { push(id, ev) }
        Task { [weak self] in
            guard let self else { return }
            if await play(id, tail) { await setStatus(id, "idle") }
        }
    }

    func cancel(_ id: String) async throws {
        guard var f = fixtures[id] else { return }
        f.cancelled = true
        fixtures[id] = f
        let turn = f.history.filter { $0.type == "turn/start" }.count
        if f.status != "idle" {
            push(id, SessionEvent(type: "turn/end", seq: 0, time: 0, data: .object(["turn": .number(Double(turn)), "reason": .object(["kind": "aborted", "reason": .object(["kind": "user"])])])))
        }
        fixtures[id]?.approval = nil
        setStatus(id, "idle")
    }

    func decide(callId: String, decision: String) async throws {
        guard let id = fixtures.first(where: { $0.value.approval?["callId"]?.string == callId })?.key,
              let next = fixtures[id]?.onApproval?[decision]?.array else { return }
        fixtures[id]?.approval = nil
        setStatus(id, "running")
        let events = next.compactMap { e -> SessionEvent? in
            guard let type = e["type"]?.string else { return nil }
            return SessionEvent(type: type, seq: 0, time: 0, data: e["data"] ?? .object([:]))
        }
        Task { [weak self] in
            guard let self else { return }
            if await play(id, events) { await setStatus(id, "idle") }
        }
    }

    func upload(data: Data, mediaType: String) async throws -> ImageAttachmentRef {
        ImageAttachmentRef(raw: .object(["attachmentId": .string("demo-\(UUID().uuidString)"), "mediaType": .string(mediaType), "bytes": .number(Double(data.count))]))
    }

    func image(_ ref: ImageAttachmentRef) async throws -> Data {
        throw ApiError.http(status: 404, message: "no image in the demo", code: nil)
    }

    func stats(_ id: String) async throws -> SessionStats {
        guard let f = fixtures[id], let s = f.stats else { throw ApiError.http(status: 404, message: "stats", code: nil) }
        let ended = Set(f.history.filter { $0.type == "turn/end" }.compactMap { $0.data["turn"]?.int })
        var raw = s
        if var o = raw.object {
            o["turns"] = .array((s["turns"]?.array ?? []).filter { ended.contains($0["turn"]?.int ?? -1) })
            raw = .object(o)
        }
        return SessionStats(raw: raw)
    }

    nonisolated func stream(sessionId: String?, after: Int) -> AsyncStream<StreamEvent> {
        AsyncStream { continuation in
            let id = UUID()
            Task { await self.subscribe(id: id, sessionId: sessionId, continuation: continuation) }
            continuation.onTermination = { _ in Task { await self.unsubscribe(id) } }
        }
    }

    private func subscribe(id: UUID, sessionId: String?, continuation: AsyncStream<StreamEvent>.Continuation) {
        subscribers[id] = (sessionId, continuation)
        continuation.yield(.open)
        guard let sessionId, var f = fixtures[sessionId], !f.started else { return }
        f.started = true
        fixtures[sessionId] = f
        Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(500))
            await self?.replay(sessionId)
        }
    }

    private func unsubscribe(_ id: UUID) {
        subscribers[id] = nil
    }

    // MARK: Replay

    private func emit(_ sessionId: String, _ message: StreamMessage) {
        for (_, sub) in subscribers where sub.sessionId == nil || sub.sessionId == sessionId {
            sub.continuation.yield(.message(message))
        }
    }

    private func push(_ id: String, _ event: SessionEvent) {
        guard var f = fixtures[id] else { return }
        f.seq += 1
        var ev = event
        ev.seq = f.seq
        ev.time = Date().timeIntervalSince1970 * 1000
        f.history.append(ev)
        fixtures[id] = f
        emit(id, .event(sessionId: id, ev))
        if ev.type == "session/title", created?.id == id, let t = ev.data["title"]?.string { created?.title = t }
    }

    private func setStatus(_ id: String, _ status: String, error: String? = nil) {
        fixtures[id]?.status = status
        emit(id, .status(sessionId: id, status: status, error: error))
    }

    private func wait(_ ms: Double) async {
        guard speed > 0 else { return }
        try? await Task.sleep(for: .milliseconds(Int(ms / speed)))
    }

    /// Streams the live tail: deltas first, then the event that settles them.
    private func play(_ id: String, _ events: [SessionEvent]) async -> Bool {
        for raw in events {
            if fixtures[id]?.cancelled ?? true { return false }
            let d = raw.data
            if raw.type == "assistant/message", speed > 0 {
                for (index, block) in (d["message"]?["content"]?.array ?? []).enumerated() {
                    let type = block["type"]?.string ?? ""
                    guard type == "text" || type == "reasoning" else { continue }
                    let text = Array(block["text"]?.string ?? "")
                    var i = 0
                    while i < text.count, !(fixtures[id]?.cancelled ?? true) {
                        let n = Int.random(in: 6...15)
                        let piece = String(text[i..<min(i + n, text.count)])
                        emit(id, .delta(DeltaMessage(sessionId: id, turn: d["turn"]?.int, step: d["step"]?.int, index: index, blockType: type, text: piece)))
                        i += n
                        await wait(type == "reasoning" ? 14 : 22)
                    }
                    await wait(120)
                }
            } else if raw.type == "tool/result" {
                let tool = d["meta"]?["tool"]?.string
                await wait(tool == "browser" || tool == "computer" || tool == "app" ? 1700 : 380)
            } else if raw.type == "step/start" {
                await wait(260)
            } else {
                await wait(30)
            }
            if fixtures[id]?.cancelled ?? true { return false }
            push(id, raw)
        }
        return true
    }

    private func replay(_ id: String) async {
        guard let f = fixtures[id] else { return }
        if f.pendingLive.isEmpty, f.approval == nil { return }
        if !f.pendingLive.isEmpty {
            setStatus(id, "running")
            fixtures[id]?.pendingLive = []
            if !(await play(id, f.pendingLive)) { return }
        }
        if let a = fixtures[id]?.approval {
            setStatus(id, "waiting-approval")
            emit(id, .approval(ApprovalMessage(sessionId: id, callId: a["callId"]?.string ?? "", tool: a["tool"]?.string ?? "",
                                               summary: a["summary"]?.string ?? "", justification: a["justification"]?.string ?? "")))
        } else {
            setStatus(id, "idle")
        }
    }
}
