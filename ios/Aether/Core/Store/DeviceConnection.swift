import Foundation

/// One paired device at runtime: tries its transports in order, keeps the client that worked, lists its chats
/// and follows their status. Every device connects on its own task, so a slow one never blocks the others.
@MainActor @Observable
final class DeviceConnection: Identifiable {
    enum Status: Hashable {
        case offline
        case connecting
        case ready
        case error(String)

        var isReady: Bool { self == .ready }
    }

    let id: String
    private(set) var device: Device
    private(set) var status: Status = .offline
    private(set) var client: (any ServerAPI)?
    private(set) var info: ServerInfo?
    private(set) var sessions: [SessionRow] = []
    private(set) var workspaces: [Workspace] = []
    private(set) var sessionsError: String?
    private(set) var loadingSessions = false
    private(set) var activeTransport: TransportKind?
    private(set) var transportLabel: String?

    private let store: DeviceStore
    private let preferences: Preferences
    private let hubSession: () -> String?
    private var transport: (any Transport)?
    private var connectTask: Task<Void, Never>?
    private var watchTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?

    init(device: Device, store: DeviceStore, preferences: Preferences, hubSession: @escaping () -> String?) {
        self.id = device.id
        self.device = device
        self.store = store
        self.preferences = preferences
        self.hubSession = hubSession
    }

    func update(device: Device) {
        self.device = device
    }

    var unreadCount: Int { sessions.filter { $0.unread && !$0.archived }.count }
    var runningCount: Int { sessions.filter { $0.status == "running" && !$0.archived }.count }
    var waitingCount: Int { sessions.filter { $0.status == "waiting-approval" && !$0.archived }.count }

    // MARK: Connect

    /// Connects (again) unless a connection is already being made. `force` drops a working one first.
    func connect(force: Bool = false) {
        if case .connecting = status, !force { return }
        if status.isReady, !force { return }
        connectTask?.cancel()
        watchTask?.cancel()
        let previous = transport
        transport = nil
        client = nil
        status = .connecting
        connectTask = Task { [weak self] in
            if let previous { await previous.close() }
            await self?.runConnect()
        }
    }

    private func makeTransports() -> [any Transport] {
        let token = store.token(for: id)
        return device.orderedTransports.compactMap { t -> (any Transport)? in
            switch t {
            case .https(let urls, let cert):
                return DirectTransport(kind: .https, urls: urls, certSha256: cert, bearer: FixedBearer(token: token), allowPlainHTTP: preferences.allowPlainHTTP)
            case .http(let urls):
                return DirectTransport(kind: .http, urls: urls, certSha256: nil, bearer: FixedBearer(token: token), allowPlainHTTP: preferences.allowPlainHTTP)
            case .ssh(let options):
                guard let pem = store.sshKey(for: id) else { return nil }
                let deviceID = id
                return SSHTunnelTransport(options: options, privateKeyPEM: pem, token: token, cachedCommandToken: store.commandToken(for: id)) { fresh in
                    Task { @MainActor in SecretStore.set(fresh, for: SecretStore.Key.commandToken(deviceID)) }
                }
            case .hub(let url, let serverId):
                return HubTransport(hubURL: url, serverId: serverId, session: hubSession())
            case .demo:
                return nil
            }
        }
    }

    private func runConnect() async {
        if device.isDemo {
            let fixture = await FixtureClient.shared()
            client = fixture
            activeTransport = .demo
            transportLabel = L.demoHint
            status = .ready
            await loadAfterConnect()
            return
        }
        var reasons: [String] = []
        for t in makeTransports() {
            if Task.isCancelled { return }
            do {
                let endpoint = try await t.open()
                if Task.isCancelled { await t.close(); return }
                let client = ServerClient(endpoint: endpoint)
                let info = try await client.info()
                self.transport = t
                self.client = client
                self.info = info
                self.activeTransport = endpoint.kind
                self.transportLabel = endpoint.label
                self.status = .ready
                store.markConnected(id, via: endpoint.kind)
                if let d = store.device(id) { device = d }
                await loadAfterConnect()
                return
            } catch {
                await t.close()
                let e = ApiError.wrap(error)
                if case .noTransport(let inner) = e {
                    reasons.append("\(t.kind.label): " + (inner.isEmpty ? e.message : inner.joined(separator: " ")))
                } else {
                    reasons.append("\(t.kind.label): \(e.message)")
                }
                if case .unauthorized = e { status = .error(e.message); return }
            }
        }
        if Task.isCancelled { return }
        status = .error(ApiError.noTransport(reasons).message)
    }

    private func loadAfterConnect() async {
        await refreshSessions()
        watchStatus()
    }

    /// Reloads the chat list; called on every visit of the Chats screen.
    func refreshSessions() async {
        guard let client else { return }
        loadingSessions = sessions.isEmpty
        defer { loadingSessions = false }
        do {
            async let rows = client.sessions()
            async let spaces = client.workspaces()
            let (r, w) = try await (rows, spaces)
            sessions = r
            workspaces = w
            sessionsError = nil
        } catch {
            let e = ApiError.wrap(error)
            sessionsError = e.message
            // A revoked token or an unreachable device is the device's state, shown on its card.
            if e.isUnreachable || e == .unauthorized, status.isReady { lost(e) }
        }
    }

    /// The connection stopped working (token revoked, device gone): show why and stop following its chats.
    private func lost(_ e: ApiError) {
        watchTask?.cancel()
        status = .error(e.message)
    }

    /// The status stream dropped: find out whether the device is gone or no longer accepts the token.
    private func checkAfterDrop() async {
        guard let client, status.isReady else { return }
        do {
            _ = try await client.info()
        } catch {
            let e = ApiError.wrap(error)
            if e == .unauthorized || e.isUnreachable { lost(e) }
        }
    }

    func refreshSoon() {
        refreshTask?.cancel()
        refreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            await self?.refreshSessions()
        }
    }

    /// Status of every chat (running dots, unread after a background run finishes), through the session-less stream.
    private func watchStatus() {
        watchTask?.cancel()
        guard let client else { return }
        watchTask = Task { [weak self] in
            for await event in client.stream(sessionId: nil, after: -1) {
                guard let self, !Task.isCancelled else { break }
                if case .retry = event { await checkAfterDrop(); continue }
                guard case .message(let m) = event else { continue }
                switch m {
                case .status(let sid?, let status, _):
                    let known = sessions.contains { $0.id == sid }
                    if let i = sessions.firstIndex(where: { $0.id == sid }) { sessions[i].status = status }
                    if !known || status == "idle" || status == "error" { refreshSoon() }
                case .scheduleChanged, .event, .delta, .approval, .goal, .live, .unknown, .status:
                    break
                }
            }
        }
    }

    // MARK: Actions on chats

    func createSession(cwd: String?, preset: String?) async throws -> String {
        guard let client else { throw ApiError.offline("") }
        let id = try await client.createSession(cwd: cwd, preset: preset)
        preferences.lastProject[self.id] = cwd ?? ""
        refreshSoon()
        return id
    }

    /// Pin, archive, rename or mark a chat; the list updates before the server answers.
    func patchSession(_ sessionID: String, patch: [String: JSONValue]) async {
        guard let client else { return }
        if let i = sessions.firstIndex(where: { $0.id == sessionID }) {
            if let t = patch["title"]?.string { sessions[i].title = t }
            if let p = patch["pinned"]?.bool { sessions[i].pinned = p }
            if let a = patch["archived"]?.bool { sessions[i].archived = a }
            if patch["seen"]?.bool == true { sessions[i].unread = false }
            if let u = patch["unread"]?.bool { sessions[i].unread = u }
        }
        do {
            if let row = try await client.updateSession(sessionID, patch: .object(patch)), let i = sessions.firstIndex(where: { $0.id == sessionID }) {
                var merged = row
                merged.status = sessions[i].status
                sessions[i] = merged
            }
        } catch {
            refreshSoon()
        }
    }

    /// `force`: tell the server even when the local row is not unread yet — a turn that just finished in the open chat
    /// is unread on the server until the phone says otherwise, and the list row may not have caught up.
    func markSeen(_ sessionID: String, force: Bool = false) {
        guard let i = sessions.firstIndex(where: { $0.id == sessionID }), force || sessions[i].unread else { return }
        Task { await patchSession(sessionID, patch: ["seen": .bool(true)]) }
    }

    func deleteSession(_ sessionID: String) async throws {
        guard let client else { return }
        try await client.deleteSession(sessionID)
        sessions.removeAll { $0.id == sessionID }
    }

    func disconnect() {
        connectTask?.cancel()
        watchTask?.cancel()
        let t = transport
        transport = nil
        client = nil
        status = .offline
        Task { await t?.close() }
    }
}
