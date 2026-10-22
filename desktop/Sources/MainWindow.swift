import AppKit
import WebKit

/// Height of the web UI's header row; the traffic lights sit centred in it.
let titlebarHeight: CGFloat = 48
private let trafficLightsInset: CGFloat = 18
/// Crossfade from one page (or snapshot) to the next.
private let fadeDuration: TimeInterval = 0.15
/// A snapshot stands in for a page that is not live yet, slightly dimmed.
private let snapshotAlpha: CGFloat = 0.55
/// Where the folder dialog opens next time (the parent of the last pick).
private let lastFolderKey = "lastProjectParent"

/// One instance's page. Pages stay alive in the background (also before they
/// were ever shown) so switching is instant.
@MainActor
final class Page {
    let instanceID: String
    let webView: PageWebView
    let handler: BridgeHandler
    /// The URL (with token) last handed to the web view.
    var loadedURL: URL?
    var failures = 0
    var committed = false
    var loads = 0
    /// Main-frame navigations so far; tells a late fallback timer it is stale.
    var navigations = 0
    /// The web UI rendered its first full frame since the last navigation
    /// (`dshxDesktop.ready()`, or a fallback for UIs without it).
    var ready = false
    /// The UI reported its appearance, so it is new enough to call `ready()`.
    var announcesReady = false
    /// What the page looked like when the window last left it.
    var snapshot: NSImage?
    /// The background it paints and whether that is the dark theme (`setAppearance`).
    var background: NSColor?
    var dark: Bool?

    init(instanceID: String, webView: PageWebView, handler: BridgeHandler) {
        self.instanceID = instanceID
        self.webView = webView
        self.handler = handler
    }

    var live: Bool { ready && loadedURL != nil }
}

/// What the window shows: a live page, a page's last snapshot, or only the background.
private enum Shown: Equatable {
    case nothing
    case page(String)
    case snapshot(String)

    var instanceID: String? {
        switch self {
        case .nothing: return nil
        case .page(let id), .snapshot(let id): return id
        }
    }
}

@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate {
    let manager: InstanceManager
    private var pages: [String: Page] = [:]
    private let root = RootView()
    private let container = NSView()
    private let snapshotView = SnapshotView()
    private let progress = ProgressLine()
    private let status = StatusView()
    private let banner = BannerView()
    private let errorBanner = ErrorBanner()
    private var shown: Shown = .nothing
    private var lastMouseDown: NSEvent?
    private var mouseMonitor: Any?
    private var zoom: CGFloat = CGFloat(max(0.5, min(2.0, UserDefaults.standard.double(forKey: "pageZoom").nonZero ?? 1)))

    var onInstancesChanged: (() -> Void)?
    var onAddInstance: (() -> Void)?
    var onEditInstances: (() -> Void)?

    init(manager: InstanceManager) {
        self.manager = manager
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1280, height: 820),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.title = "dsh-codex"
        window.minSize = NSSize(width: 640, height: 420)
        window.isMovableByWindowBackground = false
        window.tabbingMode = .disallowed
        window.collectionBehavior.insert(.fullScreenPrimary)
        window.center()
        window.setFrameAutosaveName("dsh-codex.main")
        super.init(window: window)
        window.delegate = self

        // The page's own background color, never the material grey.
        window.contentView = root
        container.wantsLayer = true
        for view in [container, progress, banner, errorBanner, status] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(view)
        }
        snapshotView.frame = container.bounds
        snapshotView.autoresizingMask = [.width, .height]
        snapshotView.alphaValue = 0
        container.addSubview(snapshotView)
        NSLayoutConstraint.activate([
            container.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            container.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            container.topAnchor.constraint(equalTo: root.topAnchor),
            container.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            progress.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            progress.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            progress.topAnchor.constraint(equalTo: root.topAnchor, constant: titlebarHeight),
            progress.heightAnchor.constraint(equalToConstant: 2),
            status.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            status.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            status.topAnchor.constraint(equalTo: root.topAnchor),
            status.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            banner.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            banner.topAnchor.constraint(equalTo: root.topAnchor, constant: titlebarHeight + 10),
            banner.widthAnchor.constraint(lessThanOrEqualTo: root.widthAnchor, constant: -40),
            errorBanner.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            errorBanner.topAnchor.constraint(equalTo: root.topAnchor, constant: titlebarHeight + 10),
            errorBanner.widthAnchor.constraint(lessThanOrEqualTo: root.widthAnchor, constant: -40),
        ])
        status.isHidden = true
        banner.isHidden = true
        errorBanner.isHidden = true
        status.onRetry = { [weak self] in self.map { $0.manager.reconnect($0.manager.currentID) } }
        status.onEdit = { [weak self] in self?.editInstances() }
        status.onSwitch = { [weak self] id in _ = self?.show(id) }
        errorBanner.onRetry = { [weak self] in self.map { $0.manager.reconnect($0.manager.currentID) } }
        errorBanner.onEdit = { [weak self] in self?.editInstances() }
        applyBackground()

        manager.onChange = { [weak self] in self?.refresh() }
        manager.onInstanceGone = { [weak self] id in self?.dropPage(id) }
        mouseMonitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
            self?.lastMouseDown = event
            return event
        }
        for name in [NSWindow.didResizeNotification, NSWindow.didEndLiveResizeNotification,
                     NSWindow.didExitFullScreenNotification, NSWindow.didBecomeKeyNotification,
                     NSWindow.didResignKeyNotification] {
            NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.layoutTrafficLights() }
            }
        }
        // AppKit lays the titlebar out again on its own schedule; put the buttons back each time.
        if let close = window.standardWindowButton(.closeButton) {
            for view in [close, close.superview?.superview].compactMap({ $0 }) {
                view.postsFrameChangedNotifications = true
                NotificationCenter.default.addObserver(forName: NSView.frameDidChangeNotification, object: view, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.layoutTrafficLights() }
                }
            }
        }
        layoutTrafficLights()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    // MARK: - Instances

    /// Shows `id`; false if there is no such instance.
    @discardableResult
    func show(_ id: String) -> Bool {
        guard manager.instance(id) != nil else { return false }
        manager.activate(id)
        return true
    }

    override func showWindow(_ sender: Any?) {
        super.showWindow(sender)
        layoutTrafficLights()
    }

    func addInstance() { onAddInstance?() }
    func editInstances() { onEditInstances?() }

    /// Brings pages, what is shown and the bridge state in line with the manager.
    func refresh() {
        let currentID = manager.currentID
        // A page for the shown instance and for every one that connects in the background.
        for instance in manager.instances where pages[instance.id] == nil {
            if instance.id == currentID || manager.existingConnection(instance.id) != nil {
                pages[instance.id] = makePage(instance.id)
            }
        }
        for (id, page) in pages {
            if let connection = manager.existingConnection(id), connection.status == .ready,
               let url = connection.pageURL, url != page.loadedURL {
                load(page, url)
            }
        }
        updateDisplay()
        if let name = manager.current?.name { window?.title = "dsh-codex – \(name)" }
        pushState()
        onInstancesChanged?()
    }

    private func load(_ page: Page, _ url: URL) {
        page.loadedURL = url
        page.committed = false
        page.loads += 1
        let attempt = page.loads
        appLog.note("\(page.instanceID): loading page")
        page.webView.load(URLRequest(url: url))
        // Watchdog: a navigation that never commits leaves an empty window.
        DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, page.loads == attempt, !page.committed, page.loadedURL == url,
                      self.pages[page.instanceID] === page else { return }
                appLog.note("\(page.instanceID): page did not commit within 15 s, loading again")
                page.webView.stopLoading()
                self.load(page, url)
            }
        }
    }

    // MARK: - Switching without a grey screen

    /// Shows the target instance once its page has rendered. Until then its
    /// last snapshot stands in, or the page shown so far stays.
    private func updateDisplay() {
        let id = manager.currentID
        let next: Shown
        if pages[id]?.live == true {
            next = .page(id)
        } else if pages[id]?.snapshot != nil {
            next = .snapshot(id)
        } else if stillThere(shown) {
            next = shown
        } else {
            next = .nothing
        }
        transition(to: next)
        updateChrome()
    }

    private func stillThere(_ state: Shown) -> Bool {
        switch state {
        case .nothing: return false
        case .page(let id): return pages[id] != nil
        case .snapshot(let id): return pages[id]?.snapshot != nil
        }
    }

    private func view(for state: Shown) -> NSView? {
        switch state {
        case .nothing: return nil
        case .page(let id): return pages[id]?.webView
        case .snapshot: return snapshotView
        }
    }

    private var reduceMotion: Bool { NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }

    private func transition(to next: Shown) {
        guard next != shown else { return }
        let previous = shown
        shown = next
        // A live page the window leaves is photographed for its next visit.
        if case .page(let id) = previous, let page = pages[id], page.live { capture(page) }
        let incoming = view(for: next)
        let outgoing = view(for: previous)
        let alpha = { if case .snapshot = next { return snapshotAlpha } else { return 1 } }() as CGFloat
        if case .snapshot(let id) = next { snapshotView.image = pages[id]?.snapshot }
        appLog.note("window shows \(String(describing: next))")
        if let incoming {
            container.addSubview(incoming, positioned: .above, relativeTo: nil)
        }
        if let incoming, incoming !== outgoing, !reduceMotion {
            incoming.alphaValue = 0
            NSAnimationContext.runAnimationGroup({ context in
                context.duration = fadeDuration
                context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
                incoming.animator().alphaValue = alpha
            }, completionHandler: { [weak self] in
                MainActor.assumeIsolated { self?.settle() }
            })
        } else {
            incoming?.alphaValue = alpha
            settle()
        }
        focusShown()
    }

    /// After a crossfade only the shown view is visible; the other pages keep
    /// running underneath (invisible, but rendered, so they are ready at once).
    private func settle() {
        let visible = view(for: shown)
        for page in pages.values where page.webView !== visible { page.webView.alphaValue = 0 }
        if snapshotView !== visible {
            snapshotView.alphaValue = 0
            snapshotView.image = nil
        }
    }

    /// Keys go to the shown page, never to an invisible one.
    private func focusShown() {
        guard let window else { return }
        let responder = window.firstResponder as? NSView
        if case .page(let id) = shown, let webView = pages[id]?.webView {
            if responder?.isDescendant(of: webView) != true { window.makeFirstResponder(webView) }
        } else if responder?.isDescendant(of: container) == true {
            window.makeFirstResponder(nil)
        }
    }

    private func capture(_ page: Page) {
        page.webView.takeSnapshot(with: nil) { image, _ in
            if let image { page.snapshot = image }
        }
    }

    /// Progress line, pill, inline error and the full-window status.
    private func updateChrome() {
        let id = manager.currentID
        let instance = manager.current
        let name = instance?.name ?? ""
        let connection = manager.existingConnection(id)
        let state = connection?.status ?? .idle
        let message = connection?.message ?? ""
        let live = pages[id]?.live == true
        let targetShown = shown.instanceID == id

        // Never connected and failed: the whole window explains why.
        let full = state == .error && !targetShown
        status.isHidden = !full
        if full {
            status.configure(
                name: name, state: state, message: message,
                instances: manager.instances.map { ($0.id, $0.name) }, current: id)
        }
        errorBanner.isHidden = full || state != .error
        if !errorBanner.isHidden { errorBanner.configure(title: "Keine Verbindung zu \(name)", detail: message) }

        progress.running = !full && state != .error && !live
        var pill: String?
        if state == .error || full {
            pill = nil
        } else if !live && (shown == .snapshot(id) || shown == .nothing) {
            if state == .ready { pill = "\(name) wird geladen …" }
            else if instance?.kind == .local { pill = message.isEmpty ? "\(name) wird gestartet …" : message }
            else { pill = "Verbinde mit \(name) …" }
        } else if live && state == .starting {
            pill = message
        }
        banner.isHidden = pill == nil
        if let pill { banner.text = pill }
        applyBackground()
    }

    // MARK: - Background and theme

    private func storedBackground(_ id: String) -> NSColor? {
        UserDefaults.standard.string(forKey: "background.\(id)").flatMap(NSColor.init(hex:))
    }

    /// The color of the page in view (or about to be), so nothing flashes grey.
    private func applyBackground() {
        let id = shown.instanceID ?? manager.currentID
        let page = pages[id]
        let color = page?.background ?? storedBackground(id) ?? NSColor(hex: NSApp.effectiveAppearance
            .bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? "#181818" : "#ffffff")!
        root.color = color
        window?.backgroundColor = color
        if let dark = page?.dark {
            window?.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        } else if let stored = UserDefaults.standard.string(forKey: "theme.\(id)") {
            window?.appearance = NSAppearance(named: stored == "dark" ? .darkAqua : .aqua)
        }
    }

    /// `dshxDesktop.setAppearance({background, theme})`.
    func setAppearance(background: String, theme: String, from id: String) {
        guard let page = pages[id] else { return }
        page.announcesReady = true
        if let color = NSColor(hex: background) {
            page.background = color
            UserDefaults.standard.set(background, forKey: "background.\(id)")
        }
        if theme == "dark" || theme == "light" {
            page.dark = theme == "dark"
            UserDefaults.standard.set(theme, forKey: "theme.\(id)")
        }
        applyBackground()
    }

    /// `dshxDesktop.ready()`: the page has rendered sidebar and chat.
    func pageReady(_ id: String) {
        guard let page = pages[id], !page.ready else { return }
        page.ready = true
        appLog.note("\(id): page ready")
        updateDisplay()
    }

    private func dropPage(_ id: String) {
        guard let page = pages.removeValue(forKey: id) else { return }
        page.webView.stopLoading()
        page.webView.configuration.userContentController.removeAllScriptMessageHandlers()
        page.webView.removeFromSuperview()
        if shown.instanceID == id { shown = .nothing }
    }

    private func makePage(_ id: String) -> Page {
        let handler = BridgeHandler(instanceID: id, controller: self)
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .default()
        config.preferences.isElementFullscreenEnabled = true
        config.applicationNameForUserAgent = "dsh-codex-desktop/\(appVersion)"
        let content = config.userContentController
        content.addScriptMessageHandler(handler, contentWorld: .page, name: "dshx")
        content.addUserScript(userScript(for: id))

        let webView = PageWebView(frame: container.bounds, configuration: config)
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.isInspectable = true
        webView.pageZoom = zoom
        webView.allowsBackForwardNavigationGestures = false
        webView.setValue(false, forKey: "drawsBackground")
        // Rendered in the background from the start, shown once it is ready.
        webView.alphaValue = 0
        webView.acceptsFolders = { [weak self] in self?.manager.instance(id)?.kind == .local }
        webView.onFolderDrag = { [weak self] active in self?.sendToPage(id, ["drag": ["active": active]]) }
        webView.onFolders = { [weak self] urls in
            NSApp.activate(ignoringOtherApps: true)
            self?.sendToPage(id, ["drop": ["paths": urls.map(\.path)]])
        }
        container.addSubview(webView, positioned: .below, relativeTo: nil)
        return Page(instanceID: id, webView: webView, handler: handler)
    }

    // MARK: - Bridge

    private var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
    }

    private func bridgeState(for id: String) -> [String: Any] {
        let local = manager.instance(id)?.kind == .local
        return [
            "version": appVersion,
            "current": id,
            "instances": manager.instances.map { instance -> [String: Any] in
                var row: [String: Any] = [
                    "id": instance.id, "name": instance.name, "kind": instance.kind.rawValue,
                    "active": instance.id == manager.currentID,
                    "status": manager.status(instance.id).rawValue,
                    "unread": manager.unread(instance.id),
                ]
                if let at = manager.existingConnection(instance.id)?.connectedAt {
                    row["connectedAt"] = Int(at.timeIntervalSince1970 * 1000)
                }
                return row
            },
            "capabilities": ["revealInFinder": local, "openInEditor": local, "pickFolder": local],
            "titlebar": [
                "height": titlebarHeight,
                "trafficLightsWidth": trafficLightsWidth,
                "fullScreen": window?.styleMask.contains(.fullScreen) ?? false,
            ],
        ]
    }

    private func json(_ value: Any) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: value, options: [.withoutEscapingSlashes])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    private func userScript(for id: String) -> WKUserScript {
        let source = BridgeScript.source.replacingOccurrences(of: "__DSHX_STATE__", with: json(bridgeState(for: id)))
        return WKUserScript(source: source, injectionTime: .atDocumentStart, forMainFrameOnly: true, in: .page)
    }

    /// Sends the current state to every page and refreshes the script for future loads.
    func pushState() {
        for (id, page) in pages {
            let content = page.webView.configuration.userContentController
            content.removeAllUserScripts()
            content.addUserScript(userScript(for: id))
            send(["state": bridgeState(for: id)], to: page)
        }
    }

    private func send(_ message: [String: Any], to page: Page) {
        guard page.loadedURL != nil else { return }
        page.webView.evaluateJavaScript(
            "window.__dshxDesktopReceive && window.__dshxDesktopReceive(\(json(message)))", completionHandler: nil)
    }

    private func sendToPage(_ id: String, _ message: [String: Any]) {
        if let page = pages[id] { send(message, to: page) }
    }

    /// Menu commands for the shown page (`new-chat`, `settings`, `open-project`).
    func sendCommand(_ name: String) {
        guard let page = pages[manager.currentID] else { return }
        showWindow(nil)
        send(["command": name], to: page)
    }

    /// Help menu: calls `notify` and `setBadge` from inside the shown page, as the web UI would.
    func testNotifications() {
        guard let page = pages[manager.currentID] else { return }
        let script = """
            Promise.all([
              dshxDesktop.notify('dsh-codex', 'Mitteilungen funktionieren.', { tag: 'selftest' }),
              dshxDesktop.setBadge(1)
            ]).then(function (r) { setTimeout(function () { dshxDesktop.setBadge(0); }, 4000); return 'notify ' + r[0]; })
            """
        page.webView.callAsyncJavaScript("return await (\(script))", arguments: [:], in: nil, in: .page) { result in
            switch result {
            case .success(let value): appLog.note("\(page.instanceID): bridge self-test: \(value as? String ?? "?")")
            case .failure(let error): appLog.note("\(page.instanceID): bridge self-test failed: \(error.localizedDescription)")
            }
        }
    }

    func notificationClicked(instance id: String, tag: String) {
        showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        if manager.instance(id) != nil { show(id) }
        if let page = pages[id] { send(["notification": ["tag": tag]], to: page) }
    }

    /// Only the instance's own origin may use the bridge.
    func isTrusted(origin: WKSecurityOrigin, for id: String) -> Bool {
        guard let url = pages[id]?.loadedURL ?? manager.existingConnection(id)?.pageURL else { return false }
        return origin.protocol == url.scheme && origin.host == url.host && origin.port == (url.port ?? 0)
    }

    func revealInFinder(_ path: String, from id: String) -> Bool {
        guard manager.instance(id)?.kind == .local, let url = Editor.localFile(path) else { return false }
        NSWorkspace.shared.activateFileViewerSelecting([url])
        return true
    }

    func openInEditor(_ path: String, from id: String) -> Bool {
        guard manager.instance(id)?.kind == .local, let url = Editor.localFile(path) else { return false }
        return Editor.open(url, preferred: manager.file.editor)
    }

    /// `dshxDesktop.pickFolder`: the folder dialog as a sheet (local instance
    /// only); the path, or nil when cancelled.
    func pickFolder(title: String, prompt: String, defaultPath: String, from id: String,
                    reply: @escaping @MainActor (String?) -> Void) {
        guard manager.instance(id)?.kind == .local, let window, window.attachedSheet == nil else {
            reply(nil)
            return
        }
        showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = prompt.isEmpty ? "Projekt öffnen" : prompt
        if !title.isEmpty { panel.message = title }
        let start = Editor.localFile(defaultPath)
            ?? UserDefaults.standard.string(forKey: lastFolderKey).flatMap(Editor.localFile)
        panel.directoryURL = start ?? FileManager.default.homeDirectoryForCurrentUser
        panel.beginSheetModal(for: window) { response in
            guard response == .OK, let url = panel.url else {
                reply(nil)
                return
            }
            UserDefaults.standard.set(url.deletingLastPathComponent().path, forKey: lastFolderKey)
            reply(url.path)
        }
    }

    // MARK: - Page actions (menus)

    var currentWebView: WKWebView? { pages[manager.currentID]?.webView }

    func reloadPage() {
        guard let page = pages[manager.currentID] else { return }
        if page.webView.url == nil, let url = page.loadedURL { page.webView.load(URLRequest(url: url)) } else { page.webView.reload() }
    }

    func setZoom(_ value: CGFloat?) {
        zoom = value.map { max(0.5, min(2.0, $0)) } ?? 1
        UserDefaults.standard.set(Double(zoom), forKey: "pageZoom")
        for page in pages.values { page.webView.pageZoom = zoom }
    }

    var pageZoom: CGFloat { zoom }

    // MARK: - Window chrome

    private var trafficLightsWidth: CGFloat {
        guard let zoom = window?.standardWindowButton(.zoomButton) else { return 84 }
        return ceil(zoom.frame.maxX + 14)
    }

    /// Centres the traffic lights in the 48 pt header row of the web UI.
    func layoutTrafficLights() {
        guard let window, !window.styleMask.contains(.fullScreen),
              let close = window.standardWindowButton(.closeButton),
              let mini = window.standardWindowButton(.miniaturizeButton),
              let zoom = window.standardWindowButton(.zoomButton),
              let titlebar = close.superview, let container = titlebar.superview
        else { return }
        guard !layingOut else { return }
        layingOut = true
        defer { layingOut = false }
        var frame = container.frame
        frame.size.height = titlebarHeight
        frame.origin.y = window.frame.height - titlebarHeight
        if container.frame != frame { container.frame = frame }
        let spacing = mini.frame.minX - close.frame.minX
        for (index, button) in [close, mini, zoom].enumerated() {
            let origin = NSPoint(
                x: trafficLightsInset + CGFloat(index) * (spacing > 0 ? spacing : 20),
                y: (titlebarHeight - button.frame.height) / 2)
            if button.frame.origin != origin { button.setFrameOrigin(origin) }
        }
    }

    private var layingOut = false

    /// Called from the page's mousedown in a drag area while the button is still down.
    func beginWindowDrag() {
        guard let window, let event = lastMouseDown, event.window === window,
              NSEvent.pressedMouseButtons & 1 == 1,
              ProcessInfo.processInfo.systemUptime - event.timestamp < 1
        else { return }
        window.performDrag(with: event)
    }

    func titlebarDoubleClick() {
        guard let window else { return }
        switch UserDefaults.standard.string(forKey: "AppleActionOnDoubleClick") {
        case "Minimize": window.performMiniaturize(nil)
        case "None": break
        default: window.performZoom(nil)
        }
    }

    // MARK: - NSWindowDelegate

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        // The servers keep running; the Dock icon brings the window back.
        sender.orderOut(nil)
        return false
    }

    func windowDidEnterFullScreen(_ notification: Notification) { pushState() }

    func windowDidExitFullScreen(_ notification: Notification) {
        layoutTrafficLights()
        pushState()
    }

    // MARK: - WKNavigationDelegate

    private func page(of webView: WKWebView) -> Page? { pages.values.first { $0.webView === webView } }

    func webView(
        _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        guard let url = navigationAction.request.url, let page = page(of: webView) else {
            decisionHandler(.cancel)
            return
        }
        let isMain = navigationAction.targetFrame?.isMainFrame ?? true
        if !isMain || ["about", "blob", "data"].contains(url.scheme ?? "") {
            decisionHandler(.allow)
            return
        }
        if let home = page.loadedURL, url.scheme == home.scheme, url.host == home.host, url.port == home.port {
            decisionHandler(.allow)
            return
        }
        appLog.note("\(page.instanceID): navigation to \(url.scheme ?? "")://\(url.host ?? "") opened externally")
        openExternally(url)
        decisionHandler(.cancel)
    }

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        // The shown page is about to be replaced (new token, reload): keep its picture.
        guard let page = page(of: webView), shown == .page(page.instanceID), page.live else { return }
        capture(page)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        loadFailed(webView, error)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        loadFailed(webView, error)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard let page = page(of: webView) else { return }
        page.failures = 0
        let probe = "typeof dshxDesktop === 'object' ? dshxDesktop.instances.length + ' instances, current ' + dshxDesktop.current : 'missing'"
        webView.evaluateJavaScript(probe) { result, _ in
            appLog.note("\(page.instanceID): page loaded, bridge \(result as? String ?? "missing")")
        }
        guard !page.ready else { return }
        // A UI without `ready()` (an older server) is shown shortly after loading.
        let navigation = page.navigations
        let wait: TimeInterval = page.announcesReady ? 6 : 0.8
        DispatchQueue.main.asyncAfter(deadline: .now() + wait) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, page.navigations == navigation, !page.ready, self.pages[page.instanceID] === page else { return }
                appLog.note("\(page.instanceID): no ready() after \(wait) s, showing the page")
                page.ready = true
                self.updateDisplay()
            }
        }
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        guard let page = page(of: webView) else { return }
        page.committed = true
        page.navigations += 1
        page.ready = false
        page.announcesReady = false
        updateDisplay()
    }

    private func loadFailed(_ webView: WKWebView, _ error: Error) {
        let nsError = error as NSError
        guard nsError.code != NSURLErrorCancelled, let page = page(of: webView) else { return }
        appLog.note("\(page.instanceID): page load failed: \(nsError.domain) \(nsError.code) \(nsError.localizedDescription)")
        // Usually a tunnel that just dropped: the connection's health check
        // notices that; meanwhile retry the load a few times.
        page.loadedURL = nil
        page.failures += 1
        updateDisplay()
        guard page.failures <= 5 else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2 * Double(page.failures)) {
            MainActor.assumeIsolated { self.refresh() }
        }
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        guard let page = page(of: webView) else { return }
        appLog.note("\(page.instanceID): web content process ended, reloading")
        page.ready = false
        updateDisplay()
        if let url = page.loadedURL { load(page, url) }
    }

    // MARK: - WKUIDelegate

    func webView(
        _ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures
    ) -> WKWebView? {
        if let url = navigationAction.request.url { openExternally(url) }
        return nil
    }

    private func openExternally(_ url: URL) {
        guard ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") else { return }
        NSWorkspace.shared.open(url)
    }

    func webView(
        _ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
        initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable () -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: "OK")
        present(alert) { _ in completionHandler() }
    }

    func webView(
        _ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
        initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable (Bool) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Abbrechen")
        present(alert) { completionHandler($0 == .alertFirstButtonReturn) }
    }

    func webView(
        _ webView: WKWebView, runJavaScriptTextInputPanelWithPrompt prompt: String, defaultText: String?,
        initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable (String?) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = prompt
        let field = NSTextField(string: defaultText ?? "")
        field.frame = NSRect(x: 0, y: 0, width: 300, height: 24)
        alert.accessoryView = field
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Abbrechen")
        alert.window.initialFirstResponder = field
        present(alert) { completionHandler($0 == .alertFirstButtonReturn ? field.stringValue : nil) }
    }

    func webView(
        _ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters,
        initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable ([URL]?) -> Void
    ) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = parameters.allowsMultipleSelection
        panel.canChooseDirectories = parameters.allowsDirectories
        panel.canChooseFiles = true
        guard let window else { completionHandler(nil); return }
        panel.beginSheetModal(for: window) { completionHandler($0 == .OK ? panel.urls : nil) }
    }

    private func present(_ alert: NSAlert, _ done: @escaping (NSApplication.ModalResponse) -> Void) {
        if let window, window.isVisible, window.attachedSheet == nil {
            alert.beginSheetModal(for: window, completionHandler: done)
        } else {
            done(alert.runModal())
        }
    }
}

private extension Double {
    var nonZero: Double? { self == 0 ? nil : self }
}

extension NSColor {
    /// `#rrggbb` (sRGB), or nil.
    convenience init?(hex: String) {
        let digits = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        guard digits.count == 6, let value = UInt32(digits, radix: 16) else { return nil }
        self.init(srgbRed: CGFloat((value >> 16) & 0xff) / 255, green: CGFloat((value >> 8) & 0xff) / 255,
                  blue: CGFloat(value & 0xff) / 255, alpha: 1)
    }
}

// MARK: - Views

/// The window's content view: the page's background color behind everything.
@MainActor
final class RootView: NSView {
    var color: NSColor = .white {
        didSet { needsDisplay = true }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override var wantsUpdateLayer: Bool { true }
    override func updateLayer() { layer?.backgroundColor = color.cgColor }
}

/// An instance page. Invisible pages take no clicks, and folders dropped on
/// the local instance's page become projects (WKWebView would hand the page
/// file objects without their paths).
@MainActor
final class PageWebView: WKWebView {
    var acceptsFolders: () -> Bool = { false }
    var onFolderDrag: ((Bool) -> Void)?
    var onFolders: (([URL]) -> Void)?
    private var folderDrag = false

    override func hitTest(_ point: NSPoint) -> NSView? {
        alphaValue < 0.01 ? nil : super.hitTest(point)
    }

    /// The dragged items when all of them are folders (not packages such as apps).
    private func folders(_ info: NSDraggingInfo) -> [URL]? {
        guard acceptsFolders() else { return nil }
        let urls = info.draggingPasteboard.readObjects(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        let allFolders = urls.allSatisfy { url in
            let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .isPackageKey])
            return values?.isDirectory == true && values?.isPackage != true
        }
        return !urls.isEmpty && allFolders ? urls : nil
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        folderDrag = folders(sender) != nil
        guard folderDrag else { return super.draggingEntered(sender) }
        onFolderDrag?(true)
        return .copy
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        folderDrag ? .copy : super.draggingUpdated(sender)
    }

    override func draggingExited(_ sender: NSDraggingInfo?) {
        guard folderDrag else { return super.draggingExited(sender) }
        folderDrag = false
        onFolderDrag?(false)
    }

    override func prepareForDragOperation(_ sender: NSDraggingInfo) -> Bool {
        folderDrag ? true : super.prepareForDragOperation(sender)
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        guard folderDrag else { return super.performDragOperation(sender) }
        onFolderDrag?(false)
        guard let urls = folders(sender) else { return false }
        onFolders?(urls)
        return true
    }

    override func concludeDragOperation(_ sender: NSDraggingInfo?) {
        // WebKit never saw a folder drag, so it does not hear its end either.
        if folderDrag { folderDrag = false } else { super.concludeDragOperation(sender) }
    }
}

/// A page's last picture while it reconnects or reloads; takes the clicks
/// meant for it so nothing reaches the page underneath.
@MainActor
final class SnapshotView: NSImageView {
    override init(frame: NSRect) {
        super.init(frame: frame)
        imageScaling = .scaleNone
        imageAlignment = .alignTopLeft
        isEditable = false
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func hitTest(_ point: NSPoint) -> NSView? {
        alphaValue < 0.01 || image == nil ? nil : (frame.contains(point) ? self : nil)
    }

    override func mouseDown(with event: NSEvent) {}
    override func rightMouseDown(with event: NSEvent) {}
    override func scrollWheel(with event: NSEvent) {}
}

/// 2 pt line under the title bar while the shown instance connects or loads.
@MainActor
final class ProgressLine: NSView {
    private let bar = CALayer()

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.masksToBounds = true
        bar.cornerRadius = 1
        layer?.addSublayer(bar)
        isHidden = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    var running = false {
        didSet {
            guard running != oldValue else { return }
            isHidden = !running
            restart()
        }
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        restart()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        restart()
    }

    private func restart() {
        bar.removeAllAnimations()
        guard running, bounds.width > 0 else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            bar.backgroundColor = NSColor.controlAccentColor.cgColor
        }
        if NSWorkspace.shared.accessibilityDisplayShouldReduceMotion {
            bar.frame = bounds
            bar.opacity = 0.5
            CATransaction.commit()
            return
        }
        let width = max(80, bounds.width * 0.28)
        bar.opacity = 1
        bar.frame = CGRect(x: -width, y: 0, width: width, height: bounds.height)
        CATransaction.commit()
        let slide = CABasicAnimation(keyPath: "position.x")
        slide.fromValue = -width / 2
        slide.toValue = bounds.width + width / 2
        slide.duration = 1.2
        slide.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
        slide.repeatCount = .infinity
        bar.add(slide, forKey: "slide")
    }
}

/// Shown instead of the page when an instance never connected and failed.
@MainActor
final class StatusView: NSVisualEffectView {
    var onRetry: (() -> Void)?
    var onEdit: (() -> Void)?
    var onSwitch: ((String) -> Void)?

    private let spinner = NSProgressIndicator()
    private let title = NSTextField(labelWithString: "")
    private let detail = NSTextField(wrappingLabelWithString: "")
    private let retry = NSButton(title: "Erneut verbinden", target: nil, action: nil)
    private let edit = NSButton(title: "Instanzen bearbeiten …", target: nil, action: nil)
    private let picker = NSPopUpButton()

    override init(frame: NSRect) {
        super.init(frame: frame)
        material = .underWindowBackground
        blendingMode = .withinWindow
        state = .followsWindowActiveState

        let icon = NSImageView(image: NSApp.applicationIconImage)
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.widthAnchor.constraint(equalToConstant: 72).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 72).isActive = true
        spinner.style = .spinning
        spinner.controlSize = .small
        title.font = .systemFont(ofSize: 20, weight: .semibold)
        title.alignment = .center
        detail.alignment = .center
        detail.textColor = .secondaryLabelColor
        detail.preferredMaxLayoutWidth = 440
        detail.isSelectable = true
        retry.bezelStyle = .rounded
        retry.keyEquivalent = "\r"
        retry.target = self
        retry.action = #selector(retryClicked)
        edit.bezelStyle = .rounded
        edit.target = self
        edit.action = #selector(editClicked)
        picker.target = self
        picker.action = #selector(picked)

        let buttons = NSStackView(views: [picker, edit, retry])
        buttons.spacing = 10
        let stack = NSStackView(views: [icon, title, spinner, detail, buttons])
        stack.orientation = .vertical
        stack.spacing = 12
        stack.setCustomSpacing(18, after: detail)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.centerXAnchor.constraint(equalTo: centerXAnchor),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor, constant: -20),
            detail.widthAnchor.constraint(lessThanOrEqualToConstant: 460),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(name: String, state: ConnectionStatus, message: String,
                   instances: [(id: String, name: String)], current: String) {
        title.stringValue = name
        detail.stringValue = message.isEmpty ? (state == .error ? "Unbekannter Fehler." : "Verbindung wird aufgebaut …") : message
        detail.textColor = state == .error ? .systemRed : .secondaryLabelColor
        if state == .error { spinner.stopAnimation(nil); spinner.isHidden = true } else { spinner.isHidden = false; spinner.startAnimation(nil) }
        retry.isHidden = state != .error
        picker.removeAllItems()
        for instance in instances {
            picker.addItem(withTitle: instance.name)
            picker.lastItem?.representedObject = instance.id
        }
        if let index = instances.firstIndex(where: { $0.id == current }) { picker.selectItem(at: index) }
        picker.isHidden = instances.count < 2
    }

    @objc private func retryClicked() { onRetry?() }
    @objc private func editClicked() { onEdit?() }
    @objc private func picked() {
        if let id = picker.selectedItem?.representedObject as? String { onSwitch?(id) }
    }

    // Swallow clicks so the hidden page below never receives them.
    override func mouseDown(with event: NSEvent) {}
}

/// Small pill: "Verbinde mit VPS 2 …" over a snapshot, or a shown page reconnecting.
@MainActor
final class BannerView: NSVisualEffectView {
    private let label = NSTextField(labelWithString: "")
    private let spinner = NSProgressIndicator()

    var text: String {
        get { label.stringValue }
        set { label.stringValue = newValue.isEmpty ? "Verbindung wird wiederhergestellt …" : newValue }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        material = .popover
        blendingMode = .withinWindow
        state = .active
        wantsLayer = true
        layer?.cornerRadius = 10
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.startAnimation(nil)
        label.font = .systemFont(ofSize: 12)
        label.lineBreakMode = .byTruncatingTail
        let stack = NSStackView(views: [spinner, label])
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 6, left: 12, bottom: 6, right: 14)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    // Only a notice: clicks go to the page below.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// Connection error over a snapshot or the instance's own page: the reason,
/// "Erneut versuchen" and "Instanzen bearbeiten …".
@MainActor
final class ErrorBanner: NSVisualEffectView {
    var onRetry: (() -> Void)?
    var onEdit: (() -> Void)?

    private let title = NSTextField(labelWithString: "")
    private let detail = NSTextField(wrappingLabelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        material = .popover
        blendingMode = .withinWindow
        state = .active
        wantsLayer = true
        layer?.cornerRadius = 12
        layer?.borderWidth = 0.5
        layer?.borderColor = NSColor.separatorColor.cgColor

        let icon = NSImageView(image: NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: nil) ?? NSImage())
        icon.symbolConfiguration = .init(pointSize: 15, weight: .regular)
        icon.contentTintColor = .systemOrange
        icon.setContentHuggingPriority(.required, for: .horizontal)
        title.font = .systemFont(ofSize: 13, weight: .semibold)
        title.lineBreakMode = .byTruncatingTail
        detail.font = .systemFont(ofSize: 12)
        detail.textColor = .secondaryLabelColor
        detail.maximumNumberOfLines = 3
        detail.preferredMaxLayoutWidth = 360
        detail.isSelectable = true
        let retry = NSButton(title: "Erneut versuchen", target: self, action: #selector(retryClicked))
        let edit = NSButton(title: "Instanzen bearbeiten …", target: self, action: #selector(editClicked))
        for button in [retry, edit] {
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.setContentCompressionResistancePriority(.required, for: .horizontal)
        }
        let text = NSStackView(views: [title, detail])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 2
        let row = NSStackView(views: [icon, text, edit, retry])
        row.alignment = .centerY
        row.spacing = 10
        row.edgeInsets = NSEdgeInsets(top: 10, left: 14, bottom: 10, right: 12)
        row.setCustomSpacing(14, after: text)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor),
            row.trailingAnchor.constraint(equalTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor),
            row.bottomAnchor.constraint(equalTo: bottomAnchor),
            detail.widthAnchor.constraint(lessThanOrEqualToConstant: 360),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func configure(title: String, detail: String) {
        self.title.stringValue = title
        self.detail.stringValue = detail.isEmpty ? "Unbekannter Fehler." : detail
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }

    @objc private func retryClicked() { onRetry?() }
    @objc private func editClicked() { onEdit?() }

    override func mouseDown(with event: NSEvent) {}
}
