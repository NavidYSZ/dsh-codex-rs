import AppKit

/// Hard stop for places an agent must never operate: macOS permission and
/// authentication prompts, System Settings, keychains and password managers.
/// The model is told to ask the user instead; this enforces it.
enum Guard {
    static var extraBundleIDs: Set<String> = []

    private static let bundleIDs: Set<String> = [
        "com.apple.systempreferences",
        "com.apple.Settings",
        "com.apple.SecurityAgent",
        "com.apple.UserNotificationCenter",
        "com.apple.accessibility.universalAccessAuthWarn",
        "com.apple.coreautha",
        "com.apple.LocalAuthentication.UIAgent",
        "com.apple.loginwindow",
        "com.apple.keychainaccess",
        "com.apple.Passwords",
        "com.apple.ScreenSharing",
        "com.1password.1password",
        "com.agilebits.onepassword7",
        "com.bitwarden.desktop",
    ]

    // Some prompts come from agents without a bundle id; match by process name.
    private static let ownerNames: Set<String> = [
        "SecurityAgent", "UserNotificationCenter", "universalAccessAuthWarn", "coreautha",
        "LocalAuthenticationRemoteService", "loginwindow", "tccd", "System Settings",
        "Systemeinstellungen", "System Preferences", "Keychain Access", "Schlüsselbundverwaltung",
    ]

    private static func blocked(pid: pid_t, owner: String?) -> String? {
        if let app = NSRunningApplication(processIdentifier: pid), let id = app.bundleIdentifier,
           bundleIDs.contains(id) || extraBundleIDs.contains(id) {
            return app.localizedName ?? id
        }
        if let owner, ownerNames.contains(owner) { return owner }
        return nil
    }

    private static func windows() -> [[String: Any]] {
        (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]]) ?? []
    }

    /// The protected app owning the frontmost window under `point`, if any.
    static func blockedWindow(at point: CGPoint) -> String? {
        let me = ProcessInfo.processInfo.processIdentifier
        for info in windows() {  // front to back
            guard let pid = info[kCGWindowOwnerPID as String] as? pid_t, pid != me,
                  let boundsDict = info[kCGWindowBounds as String] as? NSDictionary,
                  let bounds = CGRect(dictionaryRepresentation: boundsDict),
                  (info[kCGWindowAlpha as String] as? Double ?? 1) > 0.01,
                  bounds.contains(point) else { continue }
            return blocked(pid: pid, owner: info[kCGWindowOwnerName as String] as? String)
        }
        return nil
    }

    /// Keyboard input goes to the focused app; also refuse while any system
    /// prompt is on screen, since those grab focus.
    static func blockedKeyboardTarget() -> String? {
        if let front = NSWorkspace.shared.frontmostApplication,
           let hit = blocked(pid: front.processIdentifier, owner: front.localizedName) {
            return hit
        }
        let me = ProcessInfo.processInfo.processIdentifier
        let prompts: Set<String> = ["SecurityAgent", "UserNotificationCenter", "universalAccessAuthWarn", "coreautha"]
        for info in windows() {
            guard let pid = info[kCGWindowOwnerPID as String] as? pid_t, pid != me,
                  let owner = info[kCGWindowOwnerName as String] as? String, prompts.contains(owner) else { continue }
            return owner
        }
        return nil
    }

    static func blockedApp(_ app: NSRunningApplication) -> String? {
        blocked(pid: app.processIdentifier, owner: app.localizedName)
    }

    static func blockedApp(named name: String) -> String? {
        let lowered = name.lowercased()
        if bundleIDs.contains(name) || extraBundleIDs.contains(name) { return name }
        let names = ["system settings", "systemeinstellungen", "system preferences", "keychain access",
                     "schlüsselbundverwaltung", "passwords", "passwörter", "1password", "bitwarden"]
        return names.contains(lowered) ? name : nil
    }

    static func error(_ what: String) -> NSError {
        NSError(domain: "dsh-cua", code: 10, userInfo: [NSLocalizedDescriptionKey:
            "Blocked: \(what) is a protected system or password surface. Do not operate it; stop and ask the user to handle it themselves."])
    }
}
