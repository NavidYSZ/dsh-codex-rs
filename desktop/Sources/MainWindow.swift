import AppKit
import WebKit

/// Height of the web UI's header row; the traffic lights sit centred in it.
let titlebarHeight: CGFloat = 48
private let trafficLightsInset: CGFloat = 18

/// One instance's page. Pages stay alive in the background so switching is instant.
@MainActor
final class Page {
    let instanceID: String
    let webView: WKWebView
    let handler: BridgeHandler
    /// The URL (with token) last handed to the web view.
    var loadedURL: URL?
    var failures = 0
    var committed = false
    var loads = 0

    init(instanceID: String, webView: WKWebView, handler: BridgeHandler) {
        self.instanceID = instanceID
        self.webView = webView
        self.handler = handler
    }
}

@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate {
    let manager: InstanceManager
    private var pages: [String: Page] = [:]
    private let container = NSView()
    private let status = StatusView()
    private let banner = BannerView()
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

        let background = NSVisualEffectView()
        background.material = .windowBackground
        background.blendingMode = .behindWindow
        background.state = .followsWindowActiveState
        window.contentView = background
        for view in [container, status, banner] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            background.addSubview(view)
        }
        NSLayoutConstraint.activate([
            container.leadingAnchor.constraint(equalTo: background.leadingAnchor),
            container.trailingAnchor.constraint(equalTo: background.trailingAnchor),
            container.topAnchor.constraint(equalTo: background.topAnchor),
            container.bottomAnchor.constraint(equalTo: background.bottomAnchor),
            status.leadingAnchor.constraint(equalTo: background.leadingAnchor),
            status.trailingAnchor.constraint(equalTo: background.trailingAnchor),
            status.topAnchor.constraint(equalTo: background.topAnchor),
            status.bottomAnchor.constraint(equalTo: background.bottomAnchor),
            banner.centerXAnchor.constraint(equalTo: background.centerXAnchor),
            banner.topAnchor.constraint(equalTo: background.topAnchor, constant: titlebarHeight + 8),
            banner.widthAnchor.constraint(lessThanOrEqualTo: background.widthAnchor, constant: -40),
        ])
        status.isHidden = true
        banner.isHidden = true
        status.onRetry = { [weak self] in self.map { $0.manager.reconnect($0.manager.currentID) } }
        status.onEdit = { [weak self] in self?.editInstances() }
        status.onSwitch = { [weak self] id in _ = self?.show(id) }

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

    /// Brings pages, overlay and the bridge state in line with the manager.
    func refresh() {
        let currentID = manager.currentID
        if pages[currentID] == nil, manager.instance(currentID) != nil { pages[currentID] = makePage(currentID) }
        for (id, page) in pages {
            page.webView.isHidden = id != currentID
            if let connection = manager.existingConnection(id), connection.status == .ready,
               let url = connection.pageURL, url != page.loadedURL {
                load(page, url)
            }
        }
        if let page = pages[currentID], window?.firstResponder == nil || window?.firstResponder === window {
            window?.makeFirstResponder(page.webView)
        }
        updateOverlay()
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

    private func updateOverlay() {
        let id = manager.currentID
        let connection = manager.existingConnection(id)
        let loaded = pages[id]?.loadedURL != nil
        let state = connection?.status ?? .idle
        let full = state == .error || (state != .ready && !loaded)
        status.isHidden = !full
        if full {
            status.configure(
                name: manager.current?.name ?? "", state: state, message: connection?.message ?? "",
                instances: manager.instances.map { ($0.id, $0.name) }, current: id)
        }
        banner.isHidden = full || state != .starting
        if !banner.isHidden { banner.text = connection?.message ?? "" }
    }

    private func dropPage(_ id: String) {
        guard let page = pages.removeValue(forKey: id) else { return }
        page.webView.stopLoading()
        page.webView.configuration.userContentController.removeAllScriptMessageHandlers()
        page.webView.removeFromSuperview()
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

        let webView = WKWebView(frame: container.bounds, configuration: config)
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.isInspectable = true
        webView.pageZoom = zoom
        webView.allowsBackForwardNavigationGestures = false
        webView.setValue(false, forKey: "drawsBackground")
        container.addSubview(webView)
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
                [
                    "id": instance.id, "name": instance.name, "kind": instance.kind.rawValue,
                    "active": instance.id == manager.currentID,
                    "status": manager.status(instance.id).rawValue,
                ]
            },
            "capabilities": ["revealInFinder": local, "openInEditor": local],
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

    /// Menu commands for the shown page (`new-chat`, `settings`).
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
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        page(of: webView)?.committed = true
    }

    private func loadFailed(_ webView: WKWebView, _ error: Error) {
        let nsError = error as NSError
        guard nsError.code != NSURLErrorCancelled, let page = page(of: webView) else { return }
        appLog.note("\(page.instanceID): page load failed: \(nsError.domain) \(nsError.code) \(nsError.localizedDescription)")
        // Usually a tunnel that just dropped: the connection's health check
        // notices that; meanwhile retry the load a few times.
        page.loadedURL = nil
        page.failures += 1
        guard page.failures <= 5 else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2 * Double(page.failures)) {
            MainActor.assumeIsolated { self.refresh() }
        }
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        guard let page = page(of: webView) else { return }
        appLog.note("\(page.instanceID): web content process ended, reloading")
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

/// Shown instead of the page while an instance connects or after it failed.
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

/// Small notice while a loaded page reconnects.
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
}
