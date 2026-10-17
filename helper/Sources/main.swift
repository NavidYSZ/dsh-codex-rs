import AppKit
import ApplicationServices
import Carbon.HIToolbox

let helperVersion = "0.2.0"

@MainActor
final class Controller: NSObject, NSApplicationDelegate {
    private var overlay: Overlay!
    private var mini: Mini!
    private let apps = AppControl()
    private let capture = Capture()
    private let input = Input()
    private var server: SocketServer?
    private var escMonitor: Any?
    private var cancelledAt: Date?
    private var maxWidth = 1440

    static var socketPath: String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("dsh-codex/cua.sock").path
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        overlay = Overlay()
        mini = Mini()
        mini.onStop = { [weak self] in self?.cancel() }
        mini.onOpen = { [weak self] in
            guard let self else { return }
            if case .window? = self.mini.source { self.apps.reveal() } else { self.mini.request("show") }
        }
        let server = SocketServer(path: Controller.socketPath) { [weak self] request in
            guard let self else { return ["ok": false, "error": "helper shutting down"] }
            return await self.handle(request)
        }
        do { try server.start() } catch {
            NSLog("dsh-cua: cannot start socket: \(error.localizedDescription)")
            NSApp.terminate(nil)
        }
        self.server = server
        installEscMonitor()
        if !AXIsProcessTrusted() || !CGPreflightScreenCaptureAccess() { requestPermissions() }
    }

    func applicationWillTerminate(_ notification: Notification) { server?.stop() }

    private func requestPermissions() {
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        _ = AXIsProcessTrustedWithOptions([key: true] as CFDictionary)
        if !CGPreflightScreenCaptureAccess() { CGRequestScreenCaptureAccess() }
    }

    /// Esc from the user (never our own synthetic presses) stops the agent.
    private func installEscMonitor() {
        escMonitor = NSEvent.addGlobalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard event.keyCode == UInt16(kVK_Escape),
                  event.cgEvent?.getIntegerValueField(.eventSourceUserData) != syntheticEventTag else { return }
            MainActor.assumeIsolated {
                // Only the foreground mode listens to Esc; background work keeps
                // running while the user types elsewhere and stops via the mini window.
                guard let self, self.overlay.active else { return }
                self.cancel()
            }
        }
    }

    /// The user stopped the agent (Esc in the foreground, Stop in the mini window).
    private func cancel() {
        cancelledAt = Date()
        overlay.deactivate()
        mini.stopped()
    }

    private var isCancelled: Bool {
        guard let at = cancelledAt else { return false }
        if Date().timeIntervalSince(at) < 20 { return true }
        cancelledAt = nil
        return false
    }

    // MARK: protocol

    private func handle(_ req: [String: Any]) async -> [String: Any] {
        let cmd = req["cmd"] as? String ?? ""
        do {
            switch cmd {
            case "status": return status()
            case "request_permissions": requestPermissions(); return status()
            case "configure": configure(req); return ["ok": true]
            case "reset": cancelledAt = nil; return ["ok": true]
            case "hide": overlay.deactivate(); mini.hide(); return ["ok": true]
            case "mini_frame":
                if let data = req["data"] as? String { mini.setImage(base64: data) }
                return ["ok": true, "cancelled": isCancelled, "requests": mini.drainRequests()]
            case "mini_poll":
                return ["ok": true, "cancelled": isCancelled, "requests": mini.drainRequests(), "visible": mini.visible]
            case "mini_done": mini.finished(); return ["ok": true]
            case "mini_hide": mini.hide(); return ["ok": true]
            case "guard_probe":
                // Read-only: what the guard would refuse right now.
                var out: [String: Any] = ["ok": true, "keyboard": Guard.blockedKeyboardTarget() ?? NSNull()]
                if let x = req["gx"] as? Double, let y = req["gy"] as? Double {
                    out["window"] = Guard.blockedWindow(at: CGPoint(x: x, y: y)) ?? NSNull()
                }
                out["frontmost"] = NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? NSNull()
                return out
            case "render_cursor":
                let path = req["path"] as? String ?? NSTemporaryDirectory() + "dsh-cursor.png"
                return ["ok": overlay.renderPreview(to: path), "path": path]
            case "quit":
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { NSApp.terminate(nil) }
                return ["ok": true]
            default: break
            }

            if isCancelled {
                return ["ok": false, "cancelled": true,
                        "error": "The user stopped the agent (Esc or the Stop button). Stop operating the computer, browser and apps, and ask the user how to continue."]
            }

            if cmd.hasPrefix("app_") || cmd.hasPrefix("mini_") { return try await background(cmd, req) }

            if let activity = req["activity"] as? String { overlay.setActivity(activity) }

            var result: [String: Any]
            switch cmd {
            case "screenshot":
                return try await screenshot(req)
            case "cursor_position":
                result = ["ok": true]
            case "move":
                let p = try point(req)
                await overlay.travel(to: p, from: input.cursorPosition)
                if req["pointer"] as? String != "agent" { try ensureTrusted(); input.move(to: p) }
                result = ["ok": true]
            case "click":
                try ensureTrusted()
                let p = try point(req)
                let count = req["count"] as? Int ?? 1
                try guardPoint(p)
                await overlay.travel(to: p, from: input.cursorPosition)
                try guardPoint(p)
                overlay.pulse(at: p, double: count > 1)
                input.click(at: p, button: req["button"] as? String ?? "left", count: count,
                            modifiers: Input.modifiers(req["modifiers"] as? [String] ?? []))
                result = ["ok": true]
            case "drag":
                try ensureTrusted()
                let a = try point(req)
                let b = try point(req, xKey: "toX", yKey: "toY")
                try guardPoint(a)
                try guardPoint(b)
                await overlay.travel(to: a, from: input.cursorPosition)
                overlay.pulse(at: a)
                let drag = Task.detached { [input] in input.drag(from: a, to: b) }
                await overlay.travel(to: b, from: a)
                await drag.value
                result = ["ok": true]
            case "scroll":
                try ensureTrusted()
                let p = try point(req)
                try guardPoint(p)
                await overlay.travel(to: p, from: input.cursorPosition)
                input.scroll(at: p, dx: Int32(req["dx"] as? Int ?? 0), dy: Int32(req["dy"] as? Int ?? 0))
                result = ["ok": true]
            case "type":
                try ensureTrusted()
                let text = req["text"] as? String ?? ""
                try guardKeyboard()
                overlay.activate()
                await Task.detached { [input] in input.type(text) }.value
                result = ["ok": true]
            case "key":
                try ensureTrusted()
                try guardKeyboard()
                overlay.activate()
                try input.key(req["keys"] as? String ?? "")
                result = ["ok": true]
            case "open_app":
                let name = req["name"] as? String ?? ""
                if let hit = Guard.blockedApp(named: name) { throw Guard.error(hit) }
                overlay.activate()
                try await openApp(name)
                result = ["ok": true]
            default:
                return ["ok": false, "error": "unknown command: \(cmd)"]
            }

            result["cursor"] = cursorInfo()
            if req["screenshotAfter"] as? Bool == true {
                let delay = req["settle"] as? Double ?? 0.45
                try await Task.sleep(nanoseconds: UInt64(delay * 1e9))
                let shot = try await screenshot(req)
                result["screenshot"] = shot
            }
            return result
        } catch {
            return ["ok": false, "error": error.localizedDescription]
        }
    }

    // MARK: background mode (apps and the browser's mini window)

    private func background(_ cmd: String, _ req: [String: Any]) async throws -> [String: Any] {
        let activity = req["activity"] as? String
        let index = (req["index"] as? NSNumber)?.intValue
        let x = (req["x"] as? NSNumber)?.doubleValue, y = (req["y"] as? NSNumber)?.doubleValue

        switch cmd {
        case "mini_show":
            let aspect = CGSize(width: req["width"] as? Double ?? 16, height: req["height"] as? Double ?? 10)
            mini.show(title: req["title"] as? String ?? "Browser", iconImage: Mini.icon(for: req["icon"] as? String),
                      source: .frames, aspect: aspect)
            if let activity { mini.setActivity(activity) }
            return ["ok": true, "requests": mini.drainRequests()]
        case "mini_step":
            if let activity { mini.setActivity(activity) }
            if let x, let y { await mini.moveCursor(to: CGPoint(x: x, y: y), click: req["click"] as? Bool == true) }
            return ["ok": true, "requests": mini.drainRequests()]
        case "app_list":
            return ["ok": true, "apps": AppControl.runningApps()]
        case "app_state":
            try ensureTrusted()
            let state = try await apps.state(app: req["app"] as? String ?? "", window: req["window"] as? String,
                                             maxWidth: req["maxWidth"] as? Int ?? maxWidth, quality: req["quality"] as? Double ?? 0.8)
            showMini(activity: activity)
            return state
        default: break
        }

        try ensureTrusted()
        guard let target = apps.target else { throw AppError.noState }
        showMini(activity: activity)
        // Glide the mini window's cursor to the element first, like the real one.
        let aims = ["app_click", "app_type", "app_set_value", "app_scroll", "app_action"].contains(cmd)
            && (index != nil || (x != nil && y != nil))
        var result: [String: Any] = ["ok": true]
        if aims {
            let p = try apps.aim(index: index, x: x, y: y)
            let image = target.toImage(p)
            result["aim"] = ["x": Int(image.x.rounded()), "y": Int(image.y.rounded())]
            if let n = apps.normalized(p) { await mini.moveCursor(to: n, click: cmd == "app_click" || cmd == "app_action") }
        }
        switch cmd {
        case "app_click":
            result["method"] = try apps.click(index: index, x: x, y: y, count: req["count"] as? Int ?? 1,
                                              button: req["button"] as? String ?? "left")
        case "app_type":
            result["method"] = try apps.type(req["text"] as? String ?? "", index: index)
        case "app_set_value":
            guard let index else { throw AppError.unsupported("set_value needs an element index") }
            result["method"] = try apps.setValue(index: index, value: req["value"] as? String ?? "")
        case "app_key":
            result["method"] = try apps.key(req["keys"] as? String ?? "")
        case "app_scroll":
            result["method"] = try apps.scroll(index: index, x: x, y: y, direction: req["direction"] as? String ?? "down",
                                               amount: (req["amount"] as? NSNumber)?.doubleValue ?? 1)
        case "app_menu":
            let path = req["path"] as? [String] ?? []
            let (label, items) = try apps.menu(path, press: req["press"] as? Bool ?? true)
            result["method"] = items.isEmpty ? "accessibility (menu \(label))" : "listed \(label)"
            if !items.isEmpty { result["items"] = items }
        case "app_action":
            guard let index else { throw AppError.unsupported("action needs an element index") }
            result["method"] = try apps.action(index: index, name: req["action"] as? String ?? "")
        case "app_reveal":
            apps.reveal()
            result["method"] = "brought to the front"
        default:
            return ["ok": false, "error": "unknown command: \(cmd)"]
        }

        if req["stateAfter"] as? Bool == true {
            try await Task.sleep(nanoseconds: UInt64((req["settle"] as? Double ?? 0.6) * 1e9))
            result["state"] = try await apps.state(app: target.app.bundleIdentifier ?? target.name, window: String(target.windowID),
                                                   maxWidth: req["maxWidth"] as? Int ?? maxWidth, quality: req["quality"] as? Double ?? 0.8)
        }
        return result
    }

    private func showMini(activity: String?) {
        guard let t = apps.target else { return }
        let title = t.title.isEmpty || t.title == t.name ? t.name : "\(t.name) · \(t.title)"
        mini.show(title: title, iconImage: t.app.icon,
                  source: .window(t.windowID, t.frame))
        if let activity { mini.setActivity(activity) }
    }

    private func status() -> [String: Any] {
        [
            "ok": true,
            "version": helperVersion,
            "pid": ProcessInfo.processInfo.processIdentifier,
            "accessibility": AXIsProcessTrusted(),
            "screenRecording": CGPreflightScreenCaptureAccess(),
            "cursor": cursorInfo(),
            "active": overlay.active,
        ]
    }

    private func configure(_ req: [String: Any]) {
        var s = overlay.style
        if let hex = req["accent"] as? String, let c = NSColor(hex: hex) { s.accent = c }
        if let v = req["label"] as? String { s.label = v }
        if let v = req["banner"] as? String { s.banner = v }
        if let v = req["escHint"] as? String { s.escHint = v }
        if let v = req["showLabel"] as? Bool { s.showLabel = v }
        if let v = req["showGlow"] as? Bool { s.showGlow = v }
        if let v = req["showActivity"] as? Bool { s.showActivity = v }
        if let v = req["clickPulse"] as? Bool { s.clickPulse = v }
        if let v = req["idleSeconds"] as? Double { s.idleSeconds = v }
        if let v = req["maxWidth"] as? Int { maxWidth = max(640, min(4096, v)) }
        if let v = req["blockedApps"] as? [String] { Guard.extraBundleIDs = Set(v) }
        overlay.style = s
        mini.accent = s.accent
        if let m = req["mini"] as? [String: Any] {
            if let v = m["done"] as? String { mini.strings.done = v }
            if let v = m["stopped"] as? String { mini.strings.stopped = v }
            if let v = m["stop"] as? String { mini.strings.stop = v }
            if let v = m["open"] as? String { mini.strings.open = v }
            if let v = m["doneAfter"] as? Double { mini.doneAfter = v }
            if let v = m["hideAfter"] as? Double { mini.hideAfter = v }
            if let v = m["debugShareable"] as? Bool { mini.setShareable(v) }
        }
    }

    private func screenshot(_ req: [String: Any]) async throws -> [String: Any] {
        let display = (req["display"] as? Int).map { CGDirectDisplayID($0) }
        let quality = req["quality"] as? Double ?? 0.8
        var shot = try await capture.screenshot(displayID: display, maxWidth: req["maxWidth"] as? Int ?? maxWidth,
                                                jpegQuality: quality, cursor: input.cursorPosition,
                                                includeOverlay: req["includeOverlay"] as? Bool == true)
        shot["cursor"] = cursorInfo()
        return shot
    }

    private var mapping: ShotMapping {
        capture.mapping ?? capture.defaultMapping(maxWidth: maxWidth, cursor: input.cursorPosition)
    }

    private func point(_ req: [String: Any], xKey: String = "x", yKey: String = "y") throws -> CGPoint {
        guard let x = (req[xKey] as? NSNumber)?.doubleValue, let y = (req[yKey] as? NSNumber)?.doubleValue else {
            throw NSError(domain: "dsh-cua", code: 2, userInfo: [NSLocalizedDescriptionKey: "\(xKey)/\(yKey) required"])
        }
        let m = mapping
        guard x >= 0, y >= 0, x <= m.imageSize.width, y <= m.imageSize.height else {
            throw NSError(domain: "dsh-cua", code: 3, userInfo: [NSLocalizedDescriptionKey:
                "(\(Int(x)), \(Int(y))) is outside the \(Int(m.imageSize.width))×\(Int(m.imageSize.height)) screenshot"])
        }
        return m.toGlobal(x, y)
    }

    private func cursorInfo() -> [String: Any] {
        let p = mapping.toImage(overlay.position ?? input.cursorPosition)
        return ["x": Int(p.x.rounded()), "y": Int(p.y.rounded())]
    }

    private func guardPoint(_ p: CGPoint) throws {
        if let hit = Guard.blockedWindow(at: p) { throw Guard.error(hit) }
    }

    private func guardKeyboard() throws {
        if let hit = Guard.blockedKeyboardTarget() { throw Guard.error(hit) }
    }

    private func ensureTrusted() throws {
        guard AXIsProcessTrusted() else { requestPermissions(); throw InputError.noAccessibility }
    }

    private func openApp(_ name: String) async throws {
        let ws = NSWorkspace.shared
        let url = ws.urlForApplication(withBundleIdentifier: name)
            ?? ["/Applications", "/System/Applications", "/System/Applications/Utilities",
                NSHomeDirectory() + "/Applications"]
                .map { URL(fileURLWithPath: "\($0)/\(name).app") }
                .first { FileManager.default.fileExists(atPath: $0.path) }
        guard let url else {
            throw NSError(domain: "dsh-cua", code: 4, userInfo: [NSLocalizedDescriptionKey: "application not found: \(name)"])
        }
        let config = NSWorkspace.OpenConfiguration()
        config.activates = true
        _ = try await ws.openApplication(at: url, configuration: config)
        try await Task.sleep(nanoseconds: 600_000_000)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let controller = MainActor.assumeIsolated { Controller() }
app.delegate = controller
app.run()
