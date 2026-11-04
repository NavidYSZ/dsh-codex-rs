import Foundation

/// Live streaming text of the running step, folded from deltas (ui/src/state.js `flushDeltas`).
struct Deltas: Hashable, Sendable {
    struct Block: Hashable, Sendable {
        var type: String
        var text: String
        var start: Double
    }

    var key: String
    var turn: Int?
    var step: Int?
    var blocks: [Int: Block] = [:]

    var sortedBlocks: [(index: Int, block: Block)] { blocks.sorted { $0.key < $1.key }.map { ($0.key, $0.value) } }
}

struct PendingPrompt: Hashable, Sendable {
    var text: String
    var images: [ImageAttachmentRef]
    var time: Double
}

/// One open chat: its log, the transcript folded from it, the live tail, status, approvals, the live view.
@MainActor @Observable
final class ChatModel {
    enum Connection: Hashable { case connecting, ok, retry, offline }

    let sessionID: String
    let client: any ServerAPI
    let device: DeviceConnection

    private(set) var header: JSONValue = .null
    private(set) var events: [SessionEvent] = []
    private(set) var transcript = Transcript.empty
    private(set) var status = "idle"
    private(set) var statusError: String?
    private(set) var deltas: Deltas?
    private(set) var approval: ApprovalMessage?
    private(set) var live: LiveMessage?
    private(set) var liveHidden: Double?
    private(set) var pending: PendingPrompt?
    private(set) var connection: Connection = .connecting
    private(set) var loadError: String?
    private(set) var loaded = false
    private(set) var stats: SessionStats?
    var toast: String?

    private var streamTask: Task<Void, Never>?
    private var deltaQueue: [DeltaMessage] = []
    private var flushTask: Task<Void, Never>?
    private var statsTask: Task<Void, Never>?
    private var toastTask: Task<Void, Never>?

    var cwd: String { header["cwd"]?.string ?? device.sessions.first { $0.id == sessionID }?.cwd ?? "" }
    var preset: String? { header["agentPreset"]?.string }
    var isRunning: Bool { status == "running" || status == "waiting-approval" }
    var title: String { transcript.title.isEmpty ? (device.sessions.first { $0.id == sessionID }?.title ?? "") : transcript.title }

    init(sessionID: String, client: any ServerAPI, device: DeviceConnection) {
        self.sessionID = sessionID
        self.client = client
        self.device = device
    }

    // MARK: Loading and the stream

    func load() async {
        loadError = nil
        do {
            let detail = try await client.session(sessionID)
            header = detail.header
            events = detail.events
            transcript = Reduce.buildTranscript(events)
            status = detail.status
            loaded = true
            device.markSeen(sessionID)
            startStream()
            loadStats()
        } catch {
            let e = ApiError.wrap(error)
            // The screen went away while loading (navigation, a sheet closing): not an error; the next appearance loads again.
            if Task.isCancelled || e == .cancelled { return }
            loadError = e.message
            loaded = true
        }
    }

    /// The chat screen (re)appears: load once, then keep the stream running (it is stopped when the screen goes away).
    func resume() async {
        if !loaded || loadError != nil {
            await load()
        } else if streamTask == nil {
            startStream()
            loadStats()
        }
    }

    /// Reconnects the stream, resuming after the last event seen (pull to refresh, foreground).
    func reconnect() async {
        if !loaded || loadError != nil { await load(); return }
        startStream()
        loadStats()
    }

    private func startStream() {
        streamTask?.cancel()
        let after = events.last?.seq ?? -1
        connection = .connecting
        streamTask = Task { [weak self] in
            guard let self else { return }
            for await event in client.stream(sessionId: sessionID, after: after) {
                if Task.isCancelled { break }
                switch event {
                case .open: connection = .ok
                case .retry: connection = .retry
                case .message(let m): handle(m)
                }
            }
        }
    }

    func stop() {
        streamTask?.cancel()
        streamTask = nil
        flushTask?.cancel()
        statsTask?.cancel()
    }

    private func handle(_ m: StreamMessage) {
        switch m {
        case .event(let sid, let ev):
            guard sid == nil || sid == sessionID else { return }
            flushDeltas()
            append(ev)
        case .delta(let d):
            guard d.sessionId == nil || d.sessionId == sessionID else { return }
            deltaQueue.append(d)
            scheduleFlush()
        case .status(let sid, let status, let error):
            guard sid == nil || sid == sessionID else { return }
            flushDeltas()
            self.status = status
            statusError = status == "error" ? error : nil
            if status != "waiting-approval" { approval = nil }
            if status != "running" { deltas = nil }
            if status == "idle" || status == "error" {
                device.markSeen(sessionID, force: true)
                device.refreshSoon()
                loadStats()
            }
        case .approval(let a):
            guard a.sessionId == nil || a.sessionId == sessionID else { return }
            approval = a
            Haptics.warning()
        case .live(let l):
            guard l.sessionId == nil || l.sessionId == sessionID else { return }
            live = l
        case .goal, .scheduleChanged, .unknown:
            break
        }
    }

    /// Deltas arrive many times per frame; fold them a few times per second so the transcript is not re-rendered per token.
    private func scheduleFlush() {
        guard flushTask == nil else { return }
        flushTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(50))
            self?.flushTask = nil
            self?.flushDeltas()
        }
    }

    private func flushDeltas() {
        guard !deltaQueue.isEmpty else { return }
        let queue = deltaQueue
        deltaQueue = []
        var cur = deltas
        let now = Date().timeIntervalSince1970 * 1000
        for m in queue {
            let key = "\(m.turn.map(String.init) ?? "?"):\(m.step.map(String.init) ?? "?")"
            if cur == nil || cur?.key != key { cur = Deltas(key: key, turn: m.turn, step: m.step) }
            let prev = cur?.blocks[m.index]
            cur?.blocks[m.index] = Deltas.Block(type: m.blockType, text: (prev?.text ?? "") + m.text, start: prev?.start ?? now)
        }
        deltas = cur
    }

    private func append(_ ev: SessionEvent) {
        let last = events.last?.seq ?? -1
        guard ev.seq > last else { return }
        events.append(ev)
        transcript = Reduce.buildTranscript(events)
        let d = ev.data
        if ev.type == "assistant/message" || ev.type == "assistant/attempt" {
            let key = "\(d["turn"]?.int.map(String.init) ?? "?"):\(d["step"]?.int.map(String.init) ?? "?")"
            if deltas?.key == key { deltas = nil }
        }
        if ev.type == "turn/end" { deltas = nil }
        if ev.type == "user/message", d["source"]?["kind"]?.string == "user" { pending = nil }
        if ev.type == "approval/decided" { approval = nil }
        if ev.type == "session/title", let t = d["title"]?.string, !t.isEmpty {
            if let i = device.sessions.firstIndex(where: { $0.id == sessionID }) {
                Task { await device.patchSession(sessionID, patch: [:]) }
                _ = i
            }
        }
        device.markSeen(sessionID)
    }

    private func loadStats() {
        statsTask?.cancel()
        statsTask = Task { [weak self] in
            guard let self else { return }
            if let s = try? await client.stats(sessionID), !Task.isCancelled { stats = s }
        }
    }

    // MARK: Actions

    struct SendOptions: Sendable {
        var model: String?
        var effort: String?
        var sandbox: String?
        var steer: Bool
    }

    func send(text: String, images: [ImageAttachmentRef], options: SendOptions) async -> Bool {
        let busy = isRunning
        if !busy { pending = PendingPrompt(text: text, images: images, time: Date().timeIntervalSince1970 * 1000) }
        statusError = nil
        let body = PromptBody(text: text, images: images.isEmpty ? nil : images, model: options.model, effort: options.effort,
                              sandbox: options.sandbox, steer: busy && options.steer ? true : nil, timeZone: TimeZone.current.identifier)
        do {
            try await client.prompt(sessionID, body: body)
            if status == "idle" || status == "error" { status = "running" }
            return true
        } catch {
            if !busy { pending = nil }
            show(L.chatSendFailed(ApiError.wrap(error).message))
            return false
        }
    }

    func cancel() async {
        do { try await client.cancel(sessionID) } catch { show(ApiError.wrap(error).message) }
    }

    func decide(callId: String, decision: String) async {
        approval = nil
        do { try await client.decide(callId: callId, decision: decision) } catch { show(ApiError.wrap(error).message) }
    }

    func upload(data: Data, mediaType: String) async throws -> ImageAttachmentRef {
        try await client.upload(data: data, mediaType: mediaType)
    }

    func hideLive() {
        liveHidden = live?.time ?? Date().timeIntervalSince1970 * 1000
    }

    /// The pending approval from the live message, or rebuilt from `approval/asked` after a reload.
    var currentApproval: ApprovalMessage? {
        if let approval { return approval }
        guard status == "waiting-approval", let a = transcript.pendingApproval, let callId = a.callId else { return nil }
        var summary = a.toolName
        for turn in transcript.turns {
            for case .tool(let t) in turn.items where t.id == callId {
                summary = t.args["command"]?.string ?? t.args["file_path"]?.string ?? summary
            }
        }
        return ApprovalMessage(sessionId: sessionID, callId: callId, tool: a.toolName, summary: summary, justification: a.reason)
    }

    func show(_ text: String) {
        toast = text
        toastTask?.cancel()
        toastTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(5))
            if !Task.isCancelled { self?.toast = nil }
        }
    }
}
