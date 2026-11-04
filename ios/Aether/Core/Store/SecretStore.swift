import Foundation
import Security

/// The Keychain: client tokens, SSH private keys, token-command results and the hub session.
/// Nothing secret is ever written to `devices.json` or UserDefaults.
enum SecretStore {
    static let service = "de.yousufzai.aether"

    enum Key {
        static func token(_ deviceID: String) -> String { "\(deviceID).token" }
        static func sshKey(_ deviceID: String) -> String { "\(deviceID).sshKey" }
        static func commandToken(_ deviceID: String) -> String { "\(deviceID).commandToken" }
        static let hubSession = "hub.session"
    }

    static func set(_ value: String?, for account: String) {
        guard let value else { delete(account); return }
        let data = Data(value.utf8)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        let update: [String: Any] = [kSecValueData as String: data]
        let status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var add = query
            add[kSecValueData as String] = data
            add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            SecItemAdd(add as CFDictionary, nil)
        }
    }

    static func get(_ account: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let data = item as? Data else { return nil }
        return String(decoding: data, as: UTF8.self)
    }

    static func delete(_ account: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
    }

    static func removeDevice(_ deviceID: String) {
        delete(Key.token(deviceID))
        delete(Key.sshKey(deviceID))
        delete(Key.commandToken(deviceID))
    }
}
