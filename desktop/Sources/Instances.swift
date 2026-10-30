import Foundation
import Security

/// Where the desktop shell keeps its files. `Application Support/dsh-codex` is
/// shared with the computer-use helper (its `cua.sock` lives there too).
/// `DSHX_DESKTOP_SUPPORT_DIR` moves instances, run files, logs and sockets
/// into one folder, for a second copy of the app next to the installed one.
enum Paths {
    /// The `DSHX_DESKTOP_SUPPORT_DIR` override, if set.
    static let isolated: URL? = ProcessInfo.processInfo.environment["DSHX_DESKTOP_SUPPORT_DIR"]
        .flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: ($0 as NSString).expandingTildeInPath, isDirectory: true) }
    static let support: URL = isolated ?? FileManager.default
        .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("dsh-codex", isDirectory: true)
    static let instancesFile = support.appendingPathComponent("instances.json")
    /// Ready files and pid files of the local servers (0700).
    static let run = support.appendingPathComponent("run", isDirectory: true)
    static let logs: URL = isolated?.appendingPathComponent("logs", isDirectory: true) ?? FileManager.default
        .urls(for: .libraryDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("Logs/dsh-codex", isDirectory: true)
    /// SSH control sockets. Short on purpose: Unix socket paths are limited to 104 bytes.
    static let sockets: URL = isolated?.appendingPathComponent("ssh", isDirectory: true) ?? FileManager.default
        .urls(for: .cachesDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("dsh-codex/ssh", isDirectory: true)

    /// Creates `url` (and parents) with mode 0700.
    static func ensurePrivateDirectory(_ url: URL) {
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try? FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: url.path)
    }
}

enum InstanceKind: String, Codable, CaseIterable {
    /// The bundled `dsh-codex` binary, started by the app.
    case local
    /// A remote `dsh-codex web` reached through an SSH tunnel.
    case ssh
    /// Any reachable dsh-codex URL; the token lives in the keychain.
    case url

    var label: String {
        switch self {
        case .local: return "Lokal (dieser Mac)"
        case .ssh: return "SSH-Tunnel"
        case .url: return "URL"
        }
    }
}

/// One entry of `instances.json`. Fields that do not belong to `kind` stay nil.
struct Instance: Codable, Equatable, Identifiable {
    var id: String
    var name: String
    var kind: InstanceKind
    /// "Beim Start verbinden": warm the connection up at launch (nil = yes).
    var connectAtLaunch: Bool?

    // local
    /// Path to a `dsh-codex` binary; default: the one inside the app bundle.
    var binary: String?
    /// Working directory of the server process; default: the home directory.
    var workingDirectory: String?
    /// `DSHX_HOME` for the server; default: `~/.dsh-codex`.
    var dataDirectory: String?

    // ssh
    var sshConfig: String?
    var host: String?
    var remotePort: Int?
    var localPort: Int?
    /// Runs on the remote host via SSH and prints the start link (`…?token=…`) or the bare token.
    var tokenCommand: String?

    // url
    var url: String?

    static func newID() -> String { UUID().uuidString.lowercased() }

    var connectsAtLaunch: Bool { connectAtLaunch ?? true }
}

/// The whole `instances.json`.
struct InstanceFile: Codable {
    var version = 1
    /// Instance shown at launch (the last one the user picked).
    var current: String?
    /// Bundle identifier or path of the editor for `openInEditor`; default: first one installed.
    var editor: String?
    var instances: [Instance]
}

enum Defaults {
    static let vpsTokenCommand =
        "sudo -n journalctl -u deepseek-harness -b -o cat --no-pager"
        + " | grep -E '^dsh(-codex)? web: http://127\\.0\\.0\\.1:3080/\\?token=' | tail -n 1"

    static func seed() -> InstanceFile {
        InstanceFile(current: "local", instances: [
            Instance(id: "local", name: "Dieser Mac", kind: .local),
        ])
    }
}

/// Loads and saves `instances.json` (0600 in a 0700 directory).
enum InstanceStore {
    static func load() -> InstanceFile {
        Paths.ensurePrivateDirectory(Paths.support)
        guard let data = try? Data(contentsOf: Paths.instancesFile) else {
            let seed = Defaults.seed()
            save(seed)
            return seed
        }
        do {
            var file = try JSONDecoder().decode(InstanceFile.self, from: data)
            if file.instances.isEmpty { file.instances = Defaults.seed().instances }
            return file
        } catch {
            // Keep the broken file for the user and start from the defaults.
            let backup = Paths.instancesFile.appendingPathExtension("broken")
            try? FileManager.default.removeItem(at: backup)
            try? FileManager.default.moveItem(at: Paths.instancesFile, to: backup)
            NSLog("dsh-codex: instances.json unreadable (\(error.localizedDescription)), moved to \(backup.lastPathComponent)")
            let seed = Defaults.seed()
            save(seed)
            return seed
        }
    }

    static func save(_ file: InstanceFile) {
        Paths.ensurePrivateDirectory(Paths.support)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(file) else { return }
        let tmp = Paths.instancesFile.appendingPathExtension("tmp")
        try? FileManager.default.removeItem(at: tmp)
        guard FileManager.default.createFile(
            atPath: tmp.path, contents: data, attributes: [.posixPermissions: 0o600]) else { return }
        if rename(tmp.path, Paths.instancesFile.path) != 0 { try? FileManager.default.removeItem(at: tmp) }
    }
}

/// Tokens of `url` instances, one generic password per instance id.
enum Keychain {
    private static let service = "de.yousufzai.dsh-codex.instance"

    static func token(for id: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: id,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func setToken(_ token: String?, for id: String) {
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: id,
        ]
        SecItemDelete(base as CFDictionary)
        guard let token, !token.isEmpty else { return }
        var add = base
        add[kSecValueData as String] = Data(token.utf8)
        add[kSecAttrLabel as String] = "dsh-codex Instanz-Token"
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(add as CFDictionary, nil)
    }
}
