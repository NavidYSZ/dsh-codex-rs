import Foundation
import SwiftUI

/// What the app shows: devices and their connections, the hub account, routing between the screens.
@MainActor @Observable
final class AppModel {
    let store: DeviceStore
    let preferences: Preferences
    private(set) var connections: [String: DeviceConnection] = [:]

    /// Navigation: the selected device and chat (NavigationSplitView columns).
    var selectedDeviceID: String?
    var selectedChat: ChatRef?
    var showWelcome: Bool
    var showSettings = false
    var showAddDevice = false
    /// A pairing payload that arrived from outside (aether://pair#…) and waits for the user's go.
    var incomingPairing: PairingPayload?
    var incomingPairingError: String?

    // Hub account (SPEC §5).
    private(set) var hubSession: HubSession?
    private(set) var hubMe: HubMe?
    private(set) var hubServers: [HubServer] = []
    private(set) var hubError: String?
    private(set) var hubBusy = false

    struct ChatRef: Hashable, Sendable {
        var deviceID: String
        var sessionID: String
    }

    init(store: DeviceStore = DeviceStore(), preferences: Preferences = Preferences()) {
        self.store = store
        self.preferences = preferences
        showWelcome = store.devices.isEmpty && !preferences.hasSeenWelcome
        if let data = SecretStore.get(SecretStore.Key.hubSession)?.data(using: .utf8) {
            hubSession = try? JSONDecoder().decode(HubSession.self, from: data)
        }
        syncConnections()
    }

    // MARK: Devices

    var devices: [Device] { store.devices }

    func connection(for id: String) -> DeviceConnection? { connections[id] }

    private func syncConnections() {
        for d in store.devices {
            if let c = connections[d.id] { c.update(device: d) }
            else { connections[d.id] = DeviceConnection(device: d, store: store, preferences: preferences, hubSession: { [weak self] in self?.hubSession?.session }) }
        }
        for id in connections.keys where store.device(id) == nil {
            connections[id]?.disconnect()
            connections[id] = nil
        }
    }

    /// Every device connects on its own task.
    func connectAll(force: Bool = false) {
        for c in connections.values { c.connect(force: force) }
    }

    @discardableResult
    func add(payload: PairingPayload) -> Device {
        let device = store.add(payload: payload)
        syncConnections()
        connections[device.id]?.connect(force: true)
        showWelcome = false
        preferences.hasSeenWelcome = true
        return device
    }

    func addDemo() {
        let d = store.addDemo()
        syncConnections()
        connections[d.id]?.connect()
        showWelcome = false
        preferences.hasSeenWelcome = true
        selectedDeviceID = d.id
    }

    func remove(_ id: String) {
        if selectedDeviceID == id { selectedDeviceID = nil }
        if selectedChat?.deviceID == id { selectedChat = nil }
        store.remove(id)
        syncConnections()
    }

    func removeAllDevices() {
        selectedDeviceID = nil
        selectedChat = nil
        store.removeAll()
        syncConnections()
    }

    func rename(_ id: String, to name: String) {
        store.rename(id, to: name)
        syncConnections()
    }

    // MARK: Links

    /// `aether://pair#…` opened from outside (Camera app, Messages, AirDrop).
    func handle(url: URL) {
        guard url.scheme?.lowercased() == "aether" else { return }
        do {
            incomingPairing = try PairingPayload.parse(url.absoluteString)
            incomingPairingError = nil
        } catch {
            incomingPairing = nil
            incomingPairingError = (error as? PairingError) == .notALink ? L.addInvalidLink : L.addInvalidPayload
        }
    }

    // MARK: Hub

    var hubClient: HubClient? {
        guard let url = HubClient.url(preferences.hubURL) else { return nil }
        return HubClient(baseURL: url)
    }

    private func saveHubSession(_ s: HubSession?) {
        hubSession = s
        if let s, let data = try? JSONEncoder().encode(s) {
            SecretStore.set(String(decoding: data, as: UTF8.self), for: SecretStore.Key.hubSession)
        } else {
            SecretStore.delete(SecretStore.Key.hubSession)
        }
    }

    func signIn(email: String, password: String) async -> Bool {
        guard let client = hubClient else { hubError = L.settingsAccountNoHub; return false }
        hubBusy = true
        defer { hubBusy = false }
        do {
            let r = try await client.login(email: email, password: password)
            saveHubSession(HubSession(hubURL: client.baseURL.absoluteString, session: r.session, user: r.user))
            hubError = nil
            await refreshHub()
            return true
        } catch {
            let e = ApiError.wrap(error)
            if case .http(401, _, _) = e { hubError = L.settingsAccountInvalidCredentials } else { hubError = L.settingsAccountFailed(e.message) }
            return false
        }
    }

    func signUp(email: String, password: String, name: String?) async -> Bool {
        guard let client = hubClient else { hubError = L.settingsAccountNoHub; return false }
        hubBusy = true
        defer { hubBusy = false }
        do {
            let r = try await client.signup(email: email, password: password, name: name)
            saveHubSession(HubSession(hubURL: client.baseURL.absoluteString, session: r.session, user: r.user))
            hubError = nil
            await refreshHub()
            return true
        } catch {
            hubError = L.settingsAccountFailed(ApiError.wrap(error).message)
            return false
        }
    }

    func verify(code: String) async -> Bool {
        guard let client = hubClient, let s = hubSession else { return false }
        hubBusy = true
        defer { hubBusy = false }
        do {
            let user = try await client.verify(session: s.session, code: code)
            saveHubSession(HubSession(hubURL: s.hubURL, session: s.session, user: user))
            await refreshHub()
            return true
        } catch {
            hubError = ApiError.wrap(error).message
            return false
        }
    }

    func signOut() async {
        if let client = hubClient, let s = hubSession { try? await client.logout(session: s.session) }
        saveHubSession(nil)
        hubMe = nil
        hubServers = []
        hubError = nil
    }

    func refreshHub() async {
        guard let client = hubClient, let s = hubSession else { return }
        do {
            async let me = client.me(session: s.session)
            async let servers = client.servers(session: s.session)
            hubMe = try await me
            hubServers = try await servers
            hubError = nil
        } catch {
            let e = ApiError.wrap(error)
            if case .http(401, _, _) = e { saveHubSession(nil); hubMe = nil; hubServers = [] }
            hubError = e.message
        }
    }

    /// A server of the account becomes a device reached through the hub's relay.
    func addHubServer(_ server: HubServer) {
        guard let s = hubSession else { return }
        let payload = PairingPayload(server: .init(id: server.id, name: server.name, hostname: server.hostname, platform: server.platform, version: server.version),
                                     client: nil, token: nil, transports: [.hub(.init(url: s.hubURL, serverId: server.id))])
        add(payload: payload)
    }
}
