import AppKit
import ApplicationServices
import ScreenCaptureKit

// Maps an Accessibility window to its window-server id (private but long-stable).
@_silgen_name("_AXUIElementGetWindow")
private func _AXUIElementGetWindow(_ element: AXUIElement, _ id: UnsafeMutablePointer<CGWindowID>) -> AXError

enum AppError: LocalizedError {
    case notFound(String), noWindow(String), badIndex(Int), noState, secure, unsupported(String)
    var errorDescription: String? {
        switch self {
        case .notFound(let n): return "application not found: \(n)"
        case .noWindow(let n): return "\(n) has no usable window; it may be minimized to the Dock, hidden or on another Space"
        case .badIndex(let i): return "element [\(i)] is not in the latest state; call \"state\" again and use a fresh index"
        case .noState: return "call \"state\" for an app first"
        case .secure: return "refused: this is a password field. Ask the user to enter it themselves."
        case .unsupported(let what): return what
        }
    }
}

/// Operates one window of another app in the background: screenshots of
/// just that window (even when covered), an indexed accessibility outline,
/// and actions through Accessibility first. The user's pointer never moves
/// and the app is not brought to the front unless an action has no
/// Accessibility path and falls back to events posted to that process.
@MainActor
final class AppControl {
    struct Target {
        let app: NSRunningApplication
        let windowID: CGWindowID
        let window: AXUIElement?
        var frame: CGRect          // global CG points, y down
        var imageSize: CGSize      // pixels of the screenshot the model saw
        var title: String

        var name: String { app.localizedName ?? app.bundleIdentifier ?? "App" }

        func toGlobal(_ x: Double, _ y: Double) -> CGPoint {
            CGPoint(x: frame.minX + CGFloat(x) * frame.width / imageSize.width,
                    y: frame.minY + CGFloat(y) * frame.height / imageSize.height)
        }

        func toImage(_ p: CGPoint) -> CGPoint {
            CGPoint(x: (p.x - frame.minX) * imageSize.width / frame.width,
                    y: (p.y - frame.minY) * imageSize.height / frame.height)
        }

        /// 0…1 inside the window, for the mini window's cursor.
        func normalized(_ p: CGPoint) -> CGPoint {
            CGPoint(x: (p.x - frame.minX) / max(1, frame.width), y: (p.y - frame.minY) / max(1, frame.height))
        }
    }

    private(set) var target: Target?
    private var elements: [AXUIElement] = []
    private let source = CGEventSource(stateID: .privateState)

    // MARK: apps and windows

    static func runningApps() -> [[String: Any]] {
        NSWorkspace.shared.runningApplications
            .filter { $0.activationPolicy == .regular }
            .map { app -> [String: Any] in
                ["name": app.localizedName ?? "", "bundleId": app.bundleIdentifier ?? "",
                 "active": app.isActive, "hidden": app.isHidden]
            }
    }

    private static func appURL(_ name: String) -> URL? {
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: name) { return url }
        let dirs = ["/Applications", "/System/Applications", "/System/Applications/Utilities",
                    "/Applications/Utilities", NSHomeDirectory() + "/Applications"]
        for dir in dirs {
            let url = URL(fileURLWithPath: "\(dir)/\(name).app")
            if FileManager.default.fileExists(atPath: url.path) { return url }
        }
        // Localized names ("Rechner" for Calculator) only exist on running or indexed apps.
        let fm = FileManager.default
        for dir in dirs {
            guard let items = try? fm.contentsOfDirectory(atPath: dir) else { continue }
            for item in items where item.hasSuffix(".app") {
                let url = URL(fileURLWithPath: "\(dir)/\(item)")
                if fm.displayName(atPath: url.path).replacingOccurrences(of: ".app", with: "")
                    .compare(name, options: [.caseInsensitive, .diacriticInsensitive]) == .orderedSame { return url }
            }
        }
        return nil
    }

    /// A running app by name or bundle id; launched in the background if needed.
    func resolve(_ name: String) async throws -> NSRunningApplication {
        let wanted = name.trimmingCharacters(in: .whitespaces)
        let match: (NSRunningApplication) -> Bool = { app in
            if let id = app.bundleIdentifier, id.caseInsensitiveCompare(wanted) == .orderedSame { return true }
            if let n = app.localizedName, n.compare(wanted, options: [.caseInsensitive, .diacriticInsensitive]) == .orderedSame { return true }
            if let url = app.bundleURL, url.deletingPathExtension().lastPathComponent.caseInsensitiveCompare(wanted) == .orderedSame { return true }
            return false
        }
        if let app = NSWorkspace.shared.runningApplications.first(where: match) { return app }
        guard let url = AppControl.appURL(wanted) else { throw AppError.notFound(wanted) }
        if let id = Bundle(url: url)?.bundleIdentifier, let hit = Guard.blockedApp(named: id) { throw Guard.error(hit) }
        let config = NSWorkspace.OpenConfiguration()
        config.activates = false
        config.addsToRecentItems = false
        let app = try await NSWorkspace.shared.openApplication(at: url, configuration: config)
        for _ in 0..<40 where windowInfos(pid: app.processIdentifier).isEmpty {
            try await Task.sleep(nanoseconds: 150_000_000)
        }
        return app
    }

    private struct WindowInfo { let id: CGWindowID; let frame: CGRect; let title: String; let onScreen: Bool }

    private func windowInfos(pid: pid_t) -> [WindowInfo] {
        let list = (CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]) ?? []
        return list.compactMap { info in
            guard info[kCGWindowOwnerPID as String] as? pid_t == pid,
                  info[kCGWindowLayer as String] as? Int == 0,
                  let id = info[kCGWindowNumber as String] as? CGWindowID,
                  let dict = info[kCGWindowBounds as String] as? NSDictionary,
                  let frame = CGRect(dictionaryRepresentation: dict),
                  frame.width > 60, frame.height > 40 else { return nil }
            return WindowInfo(id: id, frame: frame, title: info[kCGWindowName as String] as? String ?? "",
                              onScreen: info[kCGWindowIsOnscreen as String] as? Bool ?? false)
        }
    }

    private func axWindows(_ appElement: AXUIElement) -> [(AXUIElement, CGWindowID)] {
        (attr(appElement, kAXWindowsAttribute) as? [AXUIElement] ?? []).compactMap { w in
            var id: CGWindowID = 0
            return _AXUIElementGetWindow(w, &id) == .success && id != 0 ? (w, id) : nil
        }
    }

    // MARK: state

    /// Screenshot of the app's window plus an indexed outline of what can be
    /// read or operated. Coordinates in the outline are screenshot pixels.
    func state(app name: String, window: String?, maxWidth: Int, quality: Double) async throws -> [String: Any] {
        let app = try await resolve(name)
        if let hit = Guard.blockedApp(app) { throw Guard.error(hit) }
        let appElement = AXUIElementCreateApplication(app.processIdentifier)
        AXUIElementSetMessagingTimeout(appElement, 1.5)
        // Chromium and Electron build their tree only when asked.
        AXUIElementSetAttributeValue(appElement, "AXManualAccessibility" as CFString, kCFBooleanTrue)

        let infos = windowInfos(pid: app.processIdentifier)
        let ax = axWindows(appElement)
        var chosen: WindowInfo?
        if let window, !window.isEmpty {
            chosen = infos.first { String($0.id) == window }
                ?? infos.first { $0.title.localizedCaseInsensitiveContains(window) }
        }
        if chosen == nil, let main = attr(appElement, kAXMainWindowAttribute) ?? attr(appElement, kAXFocusedWindowAttribute) {
            var id: CGWindowID = 0
            if _AXUIElementGetWindow(main as! AXUIElement, &id) == .success { chosen = infos.first { $0.id == id } }
        }
        if chosen == nil { chosen = infos.first { $0.onScreen } ?? infos.first }
        guard var info = chosen else { throw AppError.noWindow(app.localizedName ?? name) }
        let axWindow = ax.first { $0.1 == info.id }?.0

        // A minimized window has no pixels to capture; restore it without activating the app.
        if let axWindow, (attr(axWindow, kAXMinimizedAttribute) as? Bool) == true {
            AXUIElementSetAttributeValue(axWindow, kAXMinimizedAttribute as CFString, kCFBooleanFalse)
            try await Task.sleep(nanoseconds: 600_000_000)
            info = windowInfos(pid: app.processIdentifier).first { $0.id == info.id } ?? info
        }

        let image = try await AppControl.captureWindow(info.id, frame: info.frame, maxWidth: maxWidth)
        let size = CGSize(width: image.width, height: image.height)
        let t = Target(app: app, windowID: info.id, window: axWindow, frame: info.frame, imageSize: size,
                       title: info.title.isEmpty ? (axWindow.flatMap { attr($0, kAXTitleAttribute) as? String } ?? "") : info.title)
        target = t
        let outline = axWindow.map { self.outline(window: $0, target: t) } ?? ["(no accessibility tree for this window)"]

        let rep = NSBitmapImageRep(cgImage: image)
        let data = rep.representation(using: .jpeg, properties: [.compressionFactor: quality]) ?? Data()
        let others = infos.filter { $0.id != info.id }.prefix(8).map { "\($0.id): \($0.title.isEmpty ? "(untitled)" : $0.title)" }
        return [
            "ok": true,
            "app": t.name,
            "bundleId": app.bundleIdentifier ?? "",
            "window": ["id": Int(info.id), "title": t.title, "width": Int(info.frame.width), "height": Int(info.frame.height)],
            "otherWindows": Array(others),
            "frontmost": app.isActive,
            "mime": "image/jpeg",
            "data": data.base64EncodedString(),
            "width": image.width,
            "height": image.height,
            "tree": outline.joined(separator: "\n"),
            "elements": elements.count,
        ]
    }

    /// Captures one window even when other windows cover it.
    static func captureWindow(_ id: CGWindowID, frame: CGRect, maxWidth: Int) async throws -> CGImage {
        guard CGPreflightScreenCaptureAccess() else { throw CaptureError.noPermission }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        guard let window = content.windows.first(where: { $0.windowID == id }) else { throw AppError.noWindow("window \(id)") }
        let filter = SCContentFilter(desktopIndependentWindow: window)
        let scale = min(1, CGFloat(maxWidth) / max(1, frame.width))
        let config = SCStreamConfiguration()
        config.width = max(1, Int((frame.width * scale).rounded()))
        config.height = max(1, Int((frame.height * scale).rounded()))
        config.showsCursor = false
        config.ignoreShadowsSingleWindow = true
        config.captureResolution = .best
        return try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    }

    private static let listedRoles: Set<String> = [
        "AXButton", "AXCheckBox", "AXRadioButton", "AXPopUpButton", "AXMenuButton", "AXComboBox",
        "AXTextField", "AXTextArea", "AXSearchField", "AXLink", "AXSlider", "AXIncrementor", "AXMenuItem",
        "AXDisclosureTriangle", "AXStaticText", "AXImage", "AXCell", "AXRow", "AXTab", "AXColorWell",
        "AXDateField", "AXSegmentedControl", "AXStepper", "AXSwitch", "AXToggle",
    ]
    private static let containerRoles: Set<String> = ["AXRow", "AXCell"]
    private static let keyAttributes = [kAXRoleAttribute, kAXSubroleAttribute, kAXTitleAttribute, kAXDescriptionAttribute,
                                        kAXValueAttribute, kAXPositionAttribute, kAXSizeAttribute, kAXEnabledAttribute,
                                        kAXFocusedAttribute, kAXPlaceholderValueAttribute, kAXChildrenAttribute] as [CFString]

    private func outline(window: AXUIElement, target: Target, limit: Int = 260) -> [String] {
        elements = []
        var lines: [String] = []
        var seenText = Set<String>()
        let bounds = target.frame.insetBy(dx: -2, dy: -2)

        func visit(_ node: AXUIElement, _ depth: Int) {
            guard elements.count < limit, depth < 48 else { return }
            var values: CFArray?
            guard AXUIElementCopyMultipleAttributeValues(node, AppControl.keyAttributes as CFArray, [], &values) == .success,
                  let v = values as? [Any], v.count == AppControl.keyAttributes.count else { return }
            func str(_ i: Int) -> String? {
                if let s = v[i] as? String { return s.isEmpty ? nil : s }
                if let n = v[i] as? NSNumber, i == 4 { return n.stringValue }
                return nil
            }
            let role = str(0) ?? ""
            let subrole = str(1)
            var frame: CGRect?
            if CFGetTypeID(v[5] as CFTypeRef) == AXValueGetTypeID(), CFGetTypeID(v[6] as CFTypeRef) == AXValueGetTypeID() {
                var p = CGPoint.zero, s = CGSize.zero
                AXValueGetValue(v[5] as! AXValue, .cgPoint, &p)
                AXValueGetValue(v[6] as! AXValue, .cgSize, &s)
                frame = CGRect(origin: p, size: s)
            }
            if let f = frame, f.width > 0, f.height > 0, !f.intersects(bounds) { return }   // scrolled away

            let title = str(2) ?? str(3) ?? str(9)
            let value = str(4)
            let visible = (frame.map { $0.width >= 2 && $0.height >= 2 } ?? false)
            // Window chrome (close, minimize, zoom) is never listed.
            let chrome: Set<String> = ["AXCloseButton", "AXMinimizeButton", "AXZoomButton", "AXFullScreenButton"]
            var include = visible && AppControl.listedRoles.contains(role) && !chrome.contains(subrole ?? "")
            if role == "AXStaticText" {
                let text = (value ?? title ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                include = include && !text.isEmpty && !seenText.contains(text)
                if include { seenText.insert(text) }
            } else if role == "AXImage" || AppControl.containerRoles.contains(role) {
                include = include && (title != nil)
            }
            if include, let f = frame {
                let index = elements.count
                elements.append(node)
                let c = target.toImage(CGPoint(x: f.midX, y: f.midY))
                var line = "[\(index)] \(AppControl.short(role, subrole))"
                if role == "AXStaticText" {
                    line += " \"\(AppControl.clip(value ?? title ?? "", 90))\""
                } else {
                    if let title { line += " \"\(AppControl.clip(title, 70))\"" }
                    if subrole == "AXSecureTextField" { line += " (password)" }
                    else if let value, role != "AXImage" { line += " value=\"\(AppControl.clip(value, 70))\"" }
                }
                if (v[7] as? Bool) == false { line += " disabled" }
                if (v[8] as? Bool) == true { line += " focused" }
                let extra = actionNames(node).filter {
                    ["AXIncrement", "AXDecrement", "AXOpen", "AXConfirm", "AXPick", "AXShowAlternateUI"].contains($0)
                }
                if !extra.isEmpty { line += " actions=\(extra.joined(separator: ","))" }
                line += " @(\(Int(c.x)),\(Int(c.y)))"
                lines.append(line)
            }
            if let children = v[10] as? [AXUIElement] {
                for child in children.prefix(400) { visit(child, depth + 1) }
            }
        }
        visit(window, 0)
        if elements.count >= limit { lines.append("… (outline truncated at \(limit) elements; scroll or use coordinates)") }
        return lines
    }

    private static func short(_ role: String, _ subrole: String?) -> String {
        if subrole == "AXSearchField" { return "searchfield" }
        if subrole == "AXSecureTextField" { return "textfield" }
        if subrole == "AXTabButton" { return "tab" }
        if subrole == "AXSwitch" || subrole == "AXToggle" { return "switch" }
        var r = role.hasPrefix("AX") ? String(role.dropFirst(2)) : role
        r = r.prefix(1).lowercased() + r.dropFirst()
        return r == "staticText" ? "text" : r
    }

    private static func clip(_ s: String, _ n: Int) -> String {
        let one = s.replacingOccurrences(of: "\n", with: " ").replacingOccurrences(of: "\"", with: "'")
        return one.count > n ? String(one.prefix(n)) + "…" : one
    }

    // MARK: actions

    private func element(_ index: Int) throws -> AXUIElement {
        guard index >= 0, index < elements.count else { throw AppError.badIndex(index) }
        return elements[index]
    }

    private func requireTarget() throws -> Target {
        guard let target, !target.app.isTerminated else { throw AppError.noState }
        if let hit = Guard.blockedApp(target.app) { throw Guard.error(hit) }
        return target
    }

    /// Global point the action aims at, for the cursor in the mini window.
    func aim(index: Int?, x: Double?, y: Double?) throws -> CGPoint {
        let t = try requireTarget()
        if let index, let f = frame(of: try element(index)) { return CGPoint(x: f.midX, y: f.midY) }
        if let x, let y { return t.toGlobal(x, y) }
        return CGPoint(x: t.frame.midX, y: t.frame.midY)
    }

    func normalized(_ p: CGPoint) -> CGPoint? { target?.normalized(p) }

    func click(index: Int?, x: Double?, y: Double?, count: Int, button: String) throws -> String {
        let t = try requireTarget()
        let point = try aim(index: index, x: x, y: y)
        if button == "right" {
            throw AppError.unsupported("right-click menus would bring the app to the front; use \"action\" with an element's AXShowMenu or \"menu\" for menu bar commands")
        }
        var el: AXUIElement? = index.map { elements[$0] }
        if el == nil {
            var hit: AXUIElement?
            if AXUIElementCopyElementAtPosition(AXUIElementCreateApplication(t.app.processIdentifier),
                                                Float(point.x), Float(point.y), &hit) == .success { el = hit }
        }
        if let el {
            if count >= 2, actionNames(el).contains("AXOpen") { try perform(el, "AXOpen"); return "accessibility (AXOpen)" }
            if count == 1 {
                var cur: AXUIElement? = el
                for _ in 0..<4 {
                    guard let node = cur else { break }
                    let role = attr(node, kAXRoleAttribute) as? String ?? ""
                    if ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"].contains(role) {
                        AXUIElementSetAttributeValue(node, kAXFocusedAttribute as CFString, kCFBooleanTrue)
                        return "accessibility (focused the field)"
                    }
                    if actionNames(node).contains("AXPress") { try perform(node, "AXPress"); return "accessibility (AXPress)" }
                    cur = attr(node, kAXParentAttribute).map { $0 as! AXUIElement }
                }
            }
        }
        postClick(pid: t.app.processIdentifier, at: point, count: count)
        return "mouse events sent to the app (may have brought it forward)"
    }

    func setValue(index: Int, value: String) throws -> String {
        _ = try requireTarget()
        let el = try element(index)
        if isSecure(el) { throw AppError.secure }
        var settable: DarwinBoolean = false
        AXUIElementIsAttributeSettable(el, kAXValueAttribute as CFString, &settable)
        guard settable.boolValue else { throw AppError.unsupported("element [\(index)] has no settable value; click it and use \"type\" instead") }
        try check(AXUIElementSetAttributeValue(el, kAXValueAttribute as CFString, value as CFString), "set value")
        return "accessibility (AXValue)"
    }

    func type(_ text: String, index: Int?) throws -> String {
        let t = try requireTarget()
        let appElement = AXUIElementCreateApplication(t.app.processIdentifier)
        var el: AXUIElement?
        if let index {
            el = try element(index)
            AXUIElementSetAttributeValue(el!, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        } else {
            el = attr(appElement, kAXFocusedUIElementAttribute).map { $0 as! AXUIElement }
        }
        guard let el else { throw AppError.unsupported("no focused text field; pass an element index") }
        if isSecure(el) { throw AppError.secure }

        let before = attr(el, kAXValueAttribute) as? String
        var settable: DarwinBoolean = false
        AXUIElementIsAttributeSettable(el, kAXSelectedTextAttribute as CFString, &settable)
        if settable.boolValue,
           AXUIElementSetAttributeValue(el, kAXSelectedTextAttribute as CFString, text as CFString) == .success,
           (attr(el, kAXValueAttribute) as? String) != before {
            return "accessibility (inserted at the caret)"
        }
        AXUIElementIsAttributeSettable(el, kAXValueAttribute as CFString, &settable)
        if settable.boolValue, let before,
           AXUIElementSetAttributeValue(el, kAXValueAttribute as CFString, (before + text) as CFString) == .success {
            return "accessibility (appended to the value)"
        }
        postText(pid: t.app.processIdentifier, text)
        return "key events sent to the app"
    }

    func key(_ combo: String) throws -> String {
        let t = try requireTarget()
        let appElement = AXUIElementCreateApplication(t.app.processIdentifier)
        let lower = combo.lowercased()
        if let focused = attr(appElement, kAXFocusedUIElementAttribute).map({ $0 as! AXUIElement }) {
            if ["return", "enter"].contains(lower), actionNames(focused).contains("AXConfirm") {
                try perform(focused, "AXConfirm"); return "accessibility (AXConfirm)"
            }
            if ["escape", "esc"].contains(lower), actionNames(focused).contains("AXCancel") {
                try perform(focused, "AXCancel"); return "accessibility (AXCancel)"
            }
        }
        let parts = combo.split(separator: "+").map { $0.trimmingCharacters(in: .whitespaces) }
        guard let name = parts.last, let (code, shift) = Keyboard.shared.keyCode(for: name) else { throw InputError.unknownKey(combo) }
        var flags = Input.modifiers(Array(parts.dropLast()))
        if shift { flags.insert(.maskShift) }
        for down in [true, false] {
            let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
            e?.flags = flags
            post(e, to: t.app.processIdentifier)
            usleep(8_000)
        }
        return flags.contains(.maskCommand)
            ? "key event sent; background apps often ignore ⌘ shortcuts, prefer \"menu\""
            : "key event sent to the app"
    }

    func scroll(index: Int?, x: Double?, y: Double?, direction: String, amount: Double) throws -> String {
        let t = try requireTarget()
        let point = try aim(index: index, x: x, y: y)
        let vertical = ["up", "down"].contains(direction)
        let sign: Double = ["down", "right"].contains(direction) ? 1 : -1
        var node: AXUIElement? = index.map { elements[$0] }
        if node == nil {
            var hit: AXUIElement?
            if AXUIElementCopyElementAtPosition(AXUIElementCreateApplication(t.app.processIdentifier),
                                                Float(point.x), Float(point.y), &hit) == .success { node = hit }
        }
        for _ in 0..<12 {
            guard let cur = node else { break }
            if attr(cur, kAXRoleAttribute) as? String == "AXScrollArea",
               let bar = attr(cur, vertical ? kAXVerticalScrollBarAttribute : kAXHorizontalScrollBarAttribute).map({ $0 as! AXUIElement }),
               let value = (attr(bar, kAXValueAttribute) as? NSNumber)?.doubleValue {
                let next = max(0, min(1, value + sign * 0.12 * amount))
                if AXUIElementSetAttributeValue(bar, kAXValueAttribute as CFString, NSNumber(value: next)) == .success {
                    return "accessibility (scroll bar \(Int(value * 100))% → \(Int(next * 100))%)"
                }
            }
            node = attr(cur, kAXParentAttribute).map { $0 as! AXUIElement }
        }
        let lines = Int32(sign * 5 * amount)
        let e = CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2,
                        wheel1: vertical ? -lines : 0, wheel2: vertical ? 0 : -lines, wheel3: 0)
        e?.location = point
        post(e, to: t.app.processIdentifier)
        return "scroll events sent to the app"
    }

    /// Walks the menu bar by title and presses the last item, or lists a menu.
    func menu(_ path: [String], press: Bool) throws -> (String, [String]) {
        let t = try requireTarget()
        let appElement = AXUIElementCreateApplication(t.app.processIdentifier)
        guard let bar = attr(appElement, kAXMenuBarAttribute).map({ $0 as! AXUIElement }) else {
            throw AppError.unsupported("\(t.name) exposes no menu bar")
        }
        func norm(_ s: String) -> String {
            s.replacingOccurrences(of: "…", with: "").replacingOccurrences(of: "...", with: "")
                .trimmingCharacters(in: .whitespaces).lowercased()
        }
        func items(_ menuOwner: AXUIElement) -> [AXUIElement] {
            let kids = attr(menuOwner, kAXChildrenAttribute) as? [AXUIElement] ?? []
            if let menu = kids.first(where: { attr($0, kAXRoleAttribute) as? String == "AXMenu" }) {
                return attr(menu, kAXChildrenAttribute) as? [AXUIElement] ?? []
            }
            return kids
        }
        func titles(_ list: [AXUIElement]) -> [String] {
            list.compactMap { attr($0, kAXTitleAttribute) as? String }.filter { !$0.isEmpty }
        }
        var level = attr(bar, kAXChildrenAttribute) as? [AXUIElement] ?? []
        if path.isEmpty { return ("menu bar", titles(level)) }
        var node: AXUIElement?
        for (i, name) in path.enumerated() {
            guard let hit = level.first(where: { norm(attr($0, kAXTitleAttribute) as? String ?? "") == norm(name) }) else {
                throw AppError.unsupported("menu item \"\(name)\" not found; available: \(titles(level).joined(separator: ", "))")
            }
            node = hit
            if i < path.count - 1 || !press { level = items(hit) }
        }
        guard let leaf = node else { return ("", []) }
        if !press { return (path.joined(separator: " › "), titles(level)) }
        if (attr(leaf, kAXEnabledAttribute) as? Bool) == false { throw AppError.unsupported("\(path.joined(separator: " › ")) is disabled right now") }
        try perform(leaf, "AXPress")
        return (path.joined(separator: " › "), [])
    }

    func action(index: Int, name: String) throws -> String {
        _ = try requireTarget()
        let el = try element(index)
        let available = actionNames(el)
        guard available.contains(name) else {
            throw AppError.unsupported("element [\(index)] does not offer \(name); it offers \(available.joined(separator: ", "))")
        }
        try perform(el, name)
        return "accessibility (\(name))"
    }

    /// Brings the target window in front of the user (their explicit request).
    func reveal() {
        guard let t = target else { return }
        if let w = t.window { AXUIElementPerformAction(w, kAXRaiseAction as CFString) }
        t.app.activate()
    }

    // MARK: helpers

    private func attr(_ el: AXUIElement, _ name: String) -> AnyObject? {
        var value: AnyObject?
        return AXUIElementCopyAttributeValue(el, name as CFString, &value) == .success ? value : nil
    }

    private func actionNames(_ el: AXUIElement) -> [String] {
        var names: CFArray?
        return AXUIElementCopyActionNames(el, &names) == .success ? (names as? [String] ?? []) : []
    }

    private func frame(of el: AXUIElement) -> CGRect? {
        guard let pos = attr(el, kAXPositionAttribute), let size = attr(el, kAXSizeAttribute) else { return nil }
        var p = CGPoint.zero, s = CGSize.zero
        AXValueGetValue(pos as! AXValue, .cgPoint, &p)
        AXValueGetValue(size as! AXValue, .cgSize, &s)
        return CGRect(origin: p, size: s)
    }

    private func isSecure(_ el: AXUIElement) -> Bool {
        attr(el, kAXSubroleAttribute) as? String == "AXSecureTextField"
    }

    private func perform(_ el: AXUIElement, _ action: String) throws {
        try check(AXUIElementPerformAction(el, action as CFString), action)
    }

    private func check(_ err: AXError, _ what: String) throws {
        guard err == .success else { throw AppError.unsupported("\(what) failed (AXError \(err.rawValue))") }
    }

    private func post(_ event: CGEvent?, to pid: pid_t) {
        guard let event else { return }
        event.setIntegerValueField(.eventSourceUserData, value: syntheticEventTag)
        event.postToPid(pid)
    }

    private func postClick(pid: pid_t, at p: CGPoint, count: Int) {
        for i in 1...max(1, count) {
            for type in [CGEventType.leftMouseDown, .leftMouseUp] {
                let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: p, mouseButton: .left)
                e?.setIntegerValueField(.mouseEventClickState, value: Int64(i))
                post(e, to: pid)
                usleep(12_000)
            }
        }
    }

    private func postText(pid: pid_t, _ text: String) {
        let units = Array(text.utf16)
        var offset = 0
        while offset < units.count {
            let slice = Array(units[offset..<min(offset + 16, units.count)])
            for down in [true, false] {
                let e = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
                slice.withUnsafeBufferPointer { e?.keyboardSetUnicodeString(stringLength: slice.count, unicodeString: $0.baseAddress) }
                post(e, to: pid)
            }
            usleep(12_000)
            offset += 16
        }
    }
}
