import Foundation

/// The paired devices: `devices.json` in Application Support (no secrets) plus the Keychain for tokens and keys.
@MainActor @Observable
final class DeviceStore {
    private(set) var devices: [Device] = []
    private let fileURL: URL

    init(directory: URL? = nil) {
        let dir = directory ?? {
            let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            return base.appending(path: "Aether", directoryHint: .isDirectory)
        }()
        fileURL = dir.appending(path: "devices.json")
        load()
    }

    private func load() {
        guard let data = try? Data(contentsOf: fileURL) else { return }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        devices = (try? decoder.decode([Device].self, from: data)) ?? []
    }

    private func save() {
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .iso8601
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let data = try encoder.encode(devices)
            try data.write(to: fileURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {
            // The list lives in memory until the next successful write; nothing else to do on a phone.
        }
    }

    func device(_ id: String) -> Device? { devices.first { $0.id == id } }

    /// Adds the device of a pairing payload, replacing an entry with the same server id, and keeps its secrets.
    @discardableResult
    func add(payload: PairingPayload) -> Device {
        var device = Device(payload: payload)
        if let existing = self.device(device.id) {
            device.addedAt = existing.addedAt
            device.lastConnectedAt = existing.lastConnectedAt
            if payload.server.name.isEmpty { device.name = existing.name }
        }
        SecretStore.set(payload.token, for: SecretStore.Key.token(device.id))
        let sshKey = payload.transports.compactMap { if case .ssh(let s) = $0 { return s.privateKey } else { return nil } }.first
        SecretStore.set(sshKey, for: SecretStore.Key.sshKey(device.id))
        SecretStore.delete(SecretStore.Key.commandToken(device.id))
        upsert(device)
        return device
    }

    func upsert(_ device: Device) {
        if let i = devices.firstIndex(where: { $0.id == device.id }) { devices[i] = device } else { devices.append(device) }
        save()
    }

    func rename(_ id: String, to name: String) {
        guard let i = devices.firstIndex(where: { $0.id == id }) else { return }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        devices[i].name = trimmed
        save()
    }

    func markConnected(_ id: String, via kind: TransportKind, at date: Date = Date()) {
        guard let i = devices.firstIndex(where: { $0.id == id }) else { return }
        devices[i].lastTransport = kind
        devices[i].lastConnectedAt = date
        save()
    }

    func remove(_ id: String) {
        devices.removeAll { $0.id == id }
        SecretStore.removeDevice(id)
        save()
    }

    func removeAll() {
        for d in devices { SecretStore.removeDevice(d.id) }
        devices = []
        save()
    }

    /// The demo device: a recorded chat from the web UI's fixtures, no server.
    @discardableResult
    func addDemo() -> Device {
        if let d = device(Device.demoID) { return d }
        let d = Device(id: Device.demoID, name: L.demoDevice, hostname: "demo", platform: "darwin", version: "0.3.0-dev",
                       transports: [.demo], isDemo: true)
        upsert(d)
        return d
    }

    // MARK: Secrets

    func token(for id: String) -> String? { SecretStore.get(SecretStore.Key.token(id)) }
    func sshKey(for id: String) -> String? { SecretStore.get(SecretStore.Key.sshKey(id)) }
    func commandToken(for id: String) -> String? { SecretStore.get(SecretStore.Key.commandToken(id)) }
}
