import AppKit
import Foundation

/// The instance list, the shown instance and one connection per started instance.
@MainActor
final class InstanceManager {
    private(set) var file: InstanceFile
    private(set) var currentID: String
    private var connections: [String: Connection] = [:]
    /// Connections being stopped stay alive here until their processes have exited.
    private var stopping: [ObjectIdentifier: Connection] = [:]
    private var badges: [String: Int] = [:]

    /// Something in the list, a status or the shown instance changed.
    var onChange: (() -> Void)?
    /// An instance was removed or replaced; its page must go.
    var onInstanceGone: ((String) -> Void)?

    init() {
        file = InstanceStore.load()
        let ids = file.instances.map(\.id)
        currentID = file.current.flatMap { ids.contains($0) ? $0 : nil } ?? ids.first ?? "local"
    }

    var instances: [Instance] { file.instances }
    var current: Instance? { instance(currentID) }

    func instance(_ id: String) -> Instance? { file.instances.first { $0.id == id } }

    /// The connection for `id`, created and started on first use.
    @discardableResult
    func connection(_ id: String) -> Connection? {
        if let existing = connections[id] { return existing }
        guard let instance = instance(id) else { return nil }
        let connection = Connection.make(instance)
        connection.onChange = { [weak self] in self?.onChange?() }
        connections[id] = connection
        connection.start()
        return connection
    }

    func existingConnection(_ id: String) -> Connection? { connections[id] }

    func status(_ id: String) -> ConnectionStatus { connections[id]?.status ?? .idle }

    /// Unread chats the page of `id` reported (`setBadge`).
    func unread(_ id: String) -> Int { badges[id] ?? 0 }

    /// Starts the connection of `id` in the background (hover in the switcher).
    func prewarm(_ id: String) {
        guard connections[id] == nil, instance(id) != nil else { return }
        appLog.note("\(id): prewarm")
        connection(id)
    }

    /// Launch: the shown instance connects first, the others marked
    /// "Beim Start verbinden" follow after `delay` seconds.
    func prewarmAtLaunch(after delay: TimeInterval) {
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                for instance in self.instances where instance.connectsAtLaunch && instance.id != self.currentID {
                    self.prewarm(instance.id)
                }
            }
        }
    }

    func activate(_ id: String) {
        guard instance(id) != nil else { return }
        currentID = id
        file.current = id
        InstanceStore.save(file)
        connection(id)
        onChange?()
    }

    /// Adds or replaces an instance. A changed instance gets a fresh connection.
    func save(_ updated: Instance, token: String?? = nil) {
        if let index = file.instances.firstIndex(where: { $0.id == updated.id }) {
            let changed = file.instances[index] != updated || token != nil
            file.instances[index] = updated
            if updated.kind == .url, let token { Keychain.setToken(token, for: updated.id) }
            if updated.kind != .url { Keychain.setToken(nil, for: updated.id) }
            InstanceStore.save(file)
            if changed { restart(updated.id) }
        } else {
            file.instances.append(updated)
            if updated.kind == .url, let token { Keychain.setToken(token, for: updated.id) }
            InstanceStore.save(file)
        }
        onChange?()
    }

    func remove(_ id: String) {
        guard file.instances.count > 1, let index = file.instances.firstIndex(where: { $0.id == id }) else { return }
        file.instances.remove(at: index)
        Keychain.setToken(nil, for: id)
        badges[id] = nil
        if let connection = connections.removeValue(forKey: id) { retire(connection) {} }
        onInstanceGone?(id)
        if currentID == id { currentID = file.instances[0].id; connection(currentID) }
        file.current = currentID
        InstanceStore.save(file)
        updateBadge()
        onChange?()
    }

    func move(_ id: String, by offset: Int) {
        guard let index = file.instances.firstIndex(where: { $0.id == id }) else { return }
        let target = index + offset
        guard file.instances.indices.contains(target) else { return }
        file.instances.swapAt(index, target)
        InstanceStore.save(file)
        onChange?()
    }

    func setEditor(_ editor: String?) {
        file.editor = editor
        InstanceStore.save(file)
    }

    /// Stops and recreates the connection of `id` if it was running.
    func restart(_ id: String) {
        guard let old = connections.removeValue(forKey: id) else { return }
        onInstanceGone?(id)
        retire(old) { [weak self] in
            guard let self else { return }
            // It was running (shown or warmed up), so it runs again.
            self.connection(id)
            self.onChange?()
        }
    }

    /// Stops a connection that is no longer listed and keeps it alive until it has.
    private func retire(_ connection: Connection, _ done: @escaping () -> Void) {
        connection.onChange = nil
        let key = ObjectIdentifier(connection)
        stopping[key] = connection
        connection.stop { [weak self] in
            self?.stopping[key] = nil
            done()
        }
    }

    func reconnect(_ id: String) {
        if let connection = connections[id] { connection.reconnect() } else { connection(id) }
    }

    func setBadge(_ count: Int, for id: String) {
        let count = max(0, count)
        guard badges[id] != count else { return }
        badges[id] = count
        updateBadge()
        onChange?()  // the switchers show unread chats per instance
    }

    private func updateBadge() {
        let total = badges.values.reduce(0, +)
        appLog.note("dock badge \(total)")
        NSApp.dockTile.badgeLabel = total > 0 ? String(total) : nil
    }

    /// App quit: stops every connection (local servers get SIGTERM; SSH
    /// masters stay for a quick restart), then calls `done`.
    func stopAll(_ done: @escaping () -> Void) {
        let all = Array(connections.values)
        connections.removeAll()
        guard !all.isEmpty else { done(); return }
        var remaining = all.count
        for connection in all {
            connection.onChange = nil
            let key = ObjectIdentifier(connection)
            stopping[key] = connection
            connection.leave { [weak self] in
                self?.stopping[key] = nil
                remaining -= 1
                if remaining == 0 { done() }
            }
        }
    }
}
