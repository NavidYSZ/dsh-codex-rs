import AppKit
import ApplicationServices

/// Owns every mini card: where the stack sits (inside the DSH window, top
/// right), how the cards pile up, and whether they may be on screen at all.
///
/// Only one card is fully visible: the newest one at the bottom of the deck.
/// Older cards keep their header visible above it, like a stack of cards.
@MainActor
final class MiniCenter {
    /// Inset from the DSH window's top-right corner.
    static let margin: CGFloat = 16
    /// Vertical distance between two cards: exactly one header.
    static let step: CGFloat = Mini.headerHeight
    private static let fallbackBundle = "com.deepseek.dsh"

    var onStop: (() -> Void)?
    var onOpen: ((Mini) -> Void)?

    var strings = MiniStrings() { didSet { for m in items.values { m.strings = strings } } }
    var accent = NSColor(srgbRed: 0x33 / 255.0, green: 0x9c / 255.0, blue: 0xff / 255.0, alpha: 1) {
        didSet { for m in items.values { m.accent = accent } }
    }
    var doneAfter: Double = 6 { didSet { for m in items.values { m.doneAfter = doneAfter } } }
    var hideAfter: Double = 24 { didSet { for m in items.values { m.hideAfter = hideAfter } } }

    private var items: [String: Mini] = [:]
    private var order: [String] = []
    private var suppressed = true
    private var shareable = false
    private var focusApps: Set<String> = [fallbackBundle]
    private var focusTimer: Timer?

    init() {
        if let saved = UserDefaults.standard.stringArray(forKey: "miniFocusApps"), !saved.isEmpty {
            focusApps = Set(saved)
        }
        let workspace = NSWorkspace.shared.notificationCenter
        for name in [
            NSWorkspace.didActivateApplicationNotification,
            NSWorkspace.didDeactivateApplicationNotification,
            NSWorkspace.didHideApplicationNotification,
            NSWorkspace.didTerminateApplicationNotification,
        ] {
            workspace.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.refreshFocus() }
            }
        }
        NotificationCenter.default.addObserver(forName: NSApplication.didChangeScreenParametersNotification,
                                               object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.layout(animated: false) }
        }
        refreshFocus()
    }

    var count: Int { items.values.filter { $0.wantsVisible }.count }
    var visibleCount: Int { items.values.filter { $0.onScreen }.count }

    // MARK: commands

    @discardableResult
    func show(owner: String, title: String, iconImage: NSImage?, source: Mini.Source?, aspect: CGSize? = nil) -> Mini {
        // A new frame/action must never show a panel using a stale focus state.
        refreshFocus()
        let card = mini(owner)
        // The most recent card is the bottom one of the deck.
        order.removeAll { $0 == owner }
        order.append(owner)
        card.show(title: title, iconImage: iconImage, source: source, aspect: aspect)
        if focusTimer == nil {
            // Cross-process minimization/window closing does not emit our
            // NSWindow notifications. Poll only while a preview exists.
            let timer = Timer(timeInterval: 0.2, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.refreshFocus() }
            }
            RunLoop.main.add(timer, forMode: .common)
            focusTimer = timer
        }
        layout(animated: true)
        return card
    }

    func activity(owner: String, text: String) {
        items[owner]?.setActivity(text)
    }

    func image(owner: String, base64 data: String) {
        items[owner]?.setImage(base64: data)
    }

    func moveCursor(owner: String, to point: CGPoint, click: Bool) async {
        guard let card = items[owner] else { return }
        await card.moveCursor(to: point, click: click)
    }

    func finished(owner: String) { items[owner]?.finished() }

    func hide(owner: String) { items[owner]?.hide() }

    func hideAll() { for card in Array(items.values) { card.hide() } }

    func stopped() { for card in Array(items.values) where card.wantsVisible { card.stopped() } }

    func drainRequests(owner: String) -> [String] { items[owner]?.drainRequests() ?? [] }

    func setStrings(_ strings: MiniStrings) { self.strings = strings }

    func setShareable(_ on: Bool) {
        shareable = on
        for card in items.values { card.setShareable(on) }
    }

    func source(owner: String) -> Mini.Source? { items[owner]?.source }

    /// Empty/legacy "always visible" settings fall back to the Harness host.
    func setFocusApps(_ ids: [String]) {
        focusApps = Set(ids.filter { !$0.isEmpty })
        if focusApps.isEmpty { focusApps = [Self.fallbackBundle] }
        UserDefaults.standard.set(Array(focusApps).sorted(), forKey: "miniFocusApps")
        refreshFocus()
    }

    var focusBundleIDs: [String] { Array(focusApps).sorted() }

    // MARK: cards

    private func mini(_ owner: String) -> Mini {
        if let existing = items[owner] { return existing }
        let card = Mini(owner: owner)
        card.strings = strings
        card.accent = accent
        card.doneAfter = doneAfter
        card.hideAfter = hideAfter
        card.setShareable(shareable)
        card.suppressed = suppressed
        card.onStop = { [weak self] in self?.onStop?() }
        card.onOpen = { [weak self, weak card] in
            guard let self, let card else { return }
            self.onOpen?(card)
        }
        card.onDrag = { [weak self] delta in self?.drag(by: delta) }
        card.onResize = { [weak self] in self?.layout(animated: false) }
        card.onHidden = { [weak self] in self?.drop(owner) }
        items[owner] = card
        return card
    }

    private func drop(_ owner: String) {
        items.removeValue(forKey: owner)?.teardown()
        order.removeAll { $0 == owner }
        if count == 0 { focusTimer?.invalidate(); focusTimer = nil }
        layout(animated: true)
    }

    // MARK: geometry

    func layout(animated: Bool = false) {
        let anchor = stackAnchor()
        let live = order.compactMap { items[$0] }.filter { $0.wantsVisible }
        guard !live.isEmpty else { return }
        for (index, card) in live.enumerated() {
            let height = card.desiredHeight
            let top = anchor.top - CGFloat(index) * Self.step
            let rect = NSRect(x: anchor.right - Mini.width, y: top - height, width: Mini.width, height: height)
            card.occluded = index < live.count - 1
            card.applyFrame(rect, animated: animated)
        }
        guard !suppressed else { return }
        // Raise oldest to newest so the bottom card of the deck ends up in front.
        for card in live { card.applyVisibility() }
    }

    private func stackAnchor() -> (right: CGFloat, top: CGFloat) {
        let offset = UserDefaults.standard.string(forKey: "miniStackOffset").map(NSPointFromString) ?? .zero
        var right: CGFloat
        var top: CGFloat
        if let window = dshWindowRect() {
            right = window.maxX - Self.margin + offset.x
            top = window.maxY - Self.margin + offset.y
        } else {
            let frame = (NSScreen.main ?? NSScreen.screens[0]).visibleFrame
            right = frame.maxX - 20 + offset.x
            top = frame.maxY - 20 + offset.y
        }
        let probe = NSPoint(x: right - Mini.width / 2, y: top - 20)
        let screen = NSScreen.screens.first { $0.frame.contains(probe) } ?? NSScreen.main ?? NSScreen.screens[0]
        let visible = screen.visibleFrame
        right = min(max(right, visible.minX + 80), visible.maxX - 40)
        top = min(max(top, visible.minY + 40), visible.maxY - 8)
        return (right, top)
    }

    /// Frame of the frontmost DSH window in AppKit coordinates, or nil when DSH
    /// is not running (web profile) and the stack falls back to the screen.
    private func dshWindowRect() -> CGRect? {
        var ids = focusApps
        if ids.isEmpty { ids = [Self.fallbackBundle] }
        for app in NSWorkspace.shared.runningApplications where ids.contains(app.bundleIdentifier ?? "") {
            if let rect = Self.windowRect(pid: app.processIdentifier) { return rect }
        }
        return nil
    }

    private static func windowRect(pid: pid_t) -> CGRect? {
        guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements],
                                                    kCGNullWindowID) as? [[String: Any]] else { return nil }
        let primaryHeight = NSScreen.screens.first?.frame.height ?? 0
        for info in list {
            guard let owner = info[kCGWindowOwnerPID as String] as? NSNumber, owner.int32Value == pid,
                  let layer = info[kCGWindowLayer as String] as? NSNumber, layer.intValue == 0,
                  let alpha = info[kCGWindowAlpha as String] as? NSNumber, alpha.doubleValue > 0.2,
                  let bounds = info[kCGWindowBounds as String] as? [String: NSNumber],
                  let x = bounds["X"], let y = bounds["Y"],
                  let width = bounds["Width"], let height = bounds["Height"],
                  width.doubleValue > 400, height.doubleValue > 300
            else { continue }
            return CGRect(x: x.doubleValue, y: primaryHeight - y.doubleValue - height.doubleValue,
                          width: width.doubleValue, height: height.doubleValue)
        }
        return nil
    }

    private func drag(by delta: NSSize) {
        var offset = UserDefaults.standard.string(forKey: "miniStackOffset").map(NSPointFromString) ?? .zero
        offset.x += delta.width
        offset.y += delta.height
        UserDefaults.standard.set(NSStringFromPoint(offset), forKey: "miniStackOffset")
        layout(animated: false)
    }

    // MARK: focus

    private func refreshFocus() {
        let front = NSWorkspace.shared.frontmostApplication
        let eligible = front.map {
            focusApps.contains($0.bundleIdentifier ?? "") && !$0.isHidden
                && Self.hasFocusedWindow(pid: $0.processIdentifier)
                && Self.windowRect(pid: $0.processIdentifier) != nil
        } ?? false
        let blocked = !eligible
        if blocked != suppressed {
            suppressed = blocked
            for card in items.values { card.suppressed = blocked }
        }
        if !blocked { layout(animated: false) }
    }

    private static func hasFocusedWindow(pid: pid_t) -> Bool {
        // CGWindowList can still contain a window during its minimize
        // animation. AX focus/minimized state changes immediately.
        guard AXIsProcessTrusted() else { return windowRect(pid: pid) != nil }
        let app = AXUIElementCreateApplication(pid)
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute as CFString, &value) == .success,
              let value, CFGetTypeID(value) == AXUIElementGetTypeID() else { return false }
        let window = value as! AXUIElement
        var minimized: CFTypeRef?
        _ = AXUIElementCopyAttributeValue(window, kAXMinimizedAttribute as CFString, &minimized)
        return (minimized as? NSNumber)?.boolValue != true
    }
}
