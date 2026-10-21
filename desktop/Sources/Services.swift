import AppKit
import UserNotifications

/// macOS notifications for `dshxDesktop.notify`. A click brings the posting instance to the front.
final class Notifier: NSObject, UNUserNotificationCenterDelegate, @unchecked Sendable {
    static let shared = Notifier()

    /// Called on the main thread with the instance id and the tag passed to `notify`.
    @MainActor var onClick: ((String, String) -> Void)?

    @MainActor func setUp() {
        UNUserNotificationCenter.current().delegate = self
    }

    @MainActor func post(title: String, body: String, tag: String, instanceID: String, instanceName: String) {
        let center = UNUserNotificationCenter.current()
        center.requestAuthorization(options: [.alert, .sound]) { granted, _ in
            guard granted else { return }
            let content = UNMutableNotificationContent()
            content.title = title.isEmpty ? instanceName : title
            if !title.isEmpty { content.subtitle = instanceName }
            content.body = body
            content.sound = .default
            content.threadIdentifier = instanceID
            content.userInfo = ["instance": instanceID, "tag": tag]
            let id = tag.isEmpty ? UUID().uuidString : "\(instanceID):\(tag)"
            UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list, .sound])
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        let instance = info["instance"] as? String ?? ""
        let tag = info["tag"] as? String ?? ""
        DispatchQueue.main.async {
            MainActor.assumeIsolated { self.onClick?(instance, tag) }
        }
        completionHandler()
    }
}

/// Opens files for `dshxDesktop.openInEditor`.
enum Editor {
    /// Tried in order when `instances.json` names no editor.
    static let candidates = [
        "com.microsoft.VSCode", "com.todesktop.230313mzl4w4u92", "dev.zed.Zed", "com.sublimetext.4",
        "com.panic.Nova", "com.barebones.bbedit", "com.apple.TextEdit",
    ]

    static func application(preferred: String?) -> URL? {
        if let preferred, !preferred.isEmpty {
            if preferred.hasPrefix("/") || preferred.hasPrefix("~") {
                let url = URL(fileURLWithPath: (preferred as NSString).expandingTildeInPath)
                if FileManager.default.fileExists(atPath: url.path) { return url }
            } else if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: preferred) {
                return url
            }
        }
        for id in candidates {
            if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: id) { return url }
        }
        return nil
    }

    /// An absolute, existing path from the page (`/…`, `~/…` or `file://…`), or nil.
    static func localFile(_ raw: String) -> URL? {
        var path = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if path.hasPrefix("file://"), let url = URL(string: path) { path = url.path }
        path = (path as NSString).expandingTildeInPath
        guard path.hasPrefix("/") else { return nil }
        let url = URL(fileURLWithPath: path).standardizedFileURL
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    static func open(_ file: URL, preferred: String?) -> Bool {
        var isDirectory: ObjCBool = false
        FileManager.default.fileExists(atPath: file.path, isDirectory: &isDirectory)
        guard let app = application(preferred: preferred),
              !(isDirectory.boolValue && Bundle(url: app)?.bundleIdentifier == "com.apple.TextEdit")
        else {
            NSWorkspace.shared.activateFileViewerSelecting([file])
            return true
        }
        NSWorkspace.shared.open([file], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
        return true
    }
}

/// Copies the bundled "DSH Computer Use.app" to `~/Applications`, where
/// dsh-codex looks for it and where the macOS privacy grants stay attached.
enum HelperInstaller {
    static var bundled: URL? {
        let url = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/DSH Computer Use.app")
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    static var installed: URL {
        URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Applications/DSH Computer Use.app")
    }

    static func version(of app: URL) -> String? {
        Bundle(url: app)?.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
    }

    /// First launch: install the helper if none is there. An existing helper is left alone.
    static func installIfMissing() {
        guard let bundled, !FileManager.default.fileExists(atPath: installed.path) else { return }
        _ = install(from: bundled)
    }

    /// Replaces the installed helper with the bundled one.
    @discardableResult
    static func install(from source: URL) -> Error? {
        let fm = FileManager.default
        do {
            try fm.createDirectory(at: installed.deletingLastPathComponent(), withIntermediateDirectories: true)
            for app in NSRunningApplication.runningApplications(withBundleIdentifier: "de.yousufzai.dsh-computer-use") {
                app.terminate()
            }
            let staging = installed.deletingLastPathComponent().appendingPathComponent(".DSH Computer Use.app.new")
            try? fm.removeItem(at: staging)
            try fm.copyItem(at: source, to: staging)
            if fm.fileExists(atPath: installed.path) {
                _ = try fm.replaceItemAt(installed, withItemAt: staging)
            } else {
                try fm.moveItem(at: staging, to: installed)
            }
            return nil
        } catch {
            return error
        }
    }
}
