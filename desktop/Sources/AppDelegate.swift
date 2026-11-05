import AppKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private var manager: InstanceManager!
    private var main: MainWindowController!
    private var instancesWindow: InstancesWindowController?
    private let instanceMenu = NSMenu(title: "Instanz")
    private var terminating = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        Paths.ensurePrivateDirectory(Paths.support)
        let isolated = Paths.isolated != nil
        DispatchQueue.global(qos: .utility).async {
            // A second copy with its own support folder leaves ~/Applications alone.
            if !isolated { HelperInstaller.installIfMissing() }
            _ = ShellEnvironment.resolved()
        }
        manager = InstanceManager()
        main = MainWindowController(manager: manager)
        main.onInstancesChanged = { [weak self] in self?.instancesChanged() }
        main.onAddInstance = { [weak self] in self?.addInstance(nil) }
        main.onEditInstances = { [weak self] in self?.editInstances(nil) }
        Notifier.shared.setUp()
        Notifier.shared.onClick = { [weak self] id, tag in self?.main.notificationClicked(instance: id, tag: tag) }
        NSApp.mainMenu = buildMenu()
        main.showWindow(nil)
        manager.activate(manager.currentID)
        manager.prewarmAtLaunch(after: 2)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { main.showWindow(nil) }
        return true
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminating else { return .terminateLater }
        terminating = true
        let event = NSAppleEventManager.shared().currentAppleEvent
        let sender = event?.attributeDescriptor(forKeyword: keySenderPIDAttr)?.int32Value
        appLog.note("quit requested\(sender.map { " by pid \($0)" } ?? "")")
        // Stopping the local servers aborts their turns and background jobs:
        // ask first when they are busy. An unreachable server counts as idle.
        Task { @MainActor [weak self] in
            guard let self else { return }
            let work = await QuitCheck.run(self.manager)
            if !work.isEmpty {
                appLog.note("quit: \(work.chats.count) running chats, \(work.jobCount) background jobs")
                guard self.confirmQuit(work) else {
                    appLog.note("quit cancelled")
                    self.terminating = false
                    NSApp.reply(toApplicationShouldTerminate: false)
                    return
                }
            }
            self.shutDown()
        }
        return .terminateLater
    }

    /// Stops every connection, then lets the app quit.
    private func shutDown() {
        var replied = false
        let reply = {
            guard !replied else { return }
            replied = true
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        manager.stopAll { reply() }
        // Never hang on quit: local servers get SIGKILL after 6 s anyway.
        DispatchQueue.main.asyncAfter(deadline: .now() + 8) { reply() }
    }

    /// "N Chats arbeiten noch": true = quit anyway. Return cancels.
    private func confirmQuit(_ work: QuitCheck.Work) -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = work.headline
        alert.informativeText = work.details
        let quit = alert.addButton(withTitle: "Beenden")
        let cancel = alert.addButton(withTitle: "Abbrechen")
        quit.hasDestructiveAction = true
        quit.keyEquivalent = ""
        cancel.keyEquivalent = "\r"
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal() == .alertFirstButtonReturn
    }

    func applicationWillTerminate(_ notification: Notification) { appLog.flush() }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool { true }

    // MARK: - Actions

    @objc func showMainWindow(_ sender: Any?) {
        main.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc private func newChat(_ sender: Any?) { main.sendCommand("new-chat") }
    @objc private func openProject(_ sender: Any?) { main.sendCommand("open-project") }
    @objc private func openSettings(_ sender: Any?) { main.sendCommand("settings") }
    @objc private func transferFolder(_ sender: Any?) { main.transferFolder(path: nil, from: manager.currentID) }
    /// Chat menu, find and the shortcut overview: the page runs the command
    /// named by the item (ui/src/components/shortcuts.js).
    @objc private func pageCommand(_ sender: NSMenuItem) {
        guard let command = sender.representedObject as? String else { return }
        main.sendCommand(command)
    }

    @objc private func switchInstance(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        main.show(id)
        showMainWindow(nil)
    }

    @objc func addInstance(_ sender: Any?) {
        showMainWindow(nil)
        instancesController().presentEditor(for: nil, on: main.window)
    }

    @objc func editInstances(_ sender: Any?) {
        let controller = instancesController()
        controller.reload()
        controller.showWindow(nil)
        controller.window?.center()
        NSApp.activate(ignoringOtherApps: true)
    }

    private func instancesController() -> InstancesWindowController {
        if let instancesWindow { return instancesWindow }
        let controller = InstancesWindowController(manager: manager)
        instancesWindow = controller
        return controller
    }

    @objc private func reconnect(_ sender: Any?) { manager.reconnect(manager.currentID) }
    @objc private func reloadPage(_ sender: Any?) { main.reloadPage() }
    @objc private func zoomIn(_ sender: Any?) { main.setZoom(main.pageZoom + 0.1) }
    @objc private func zoomOut(_ sender: Any?) { main.setZoom(main.pageZoom - 0.1) }
    @objc private func zoomReset(_ sender: Any?) { main.setZoom(nil) }

    @objc private func testNotifications(_ sender: Any?) { main.testNotifications() }

    @objc private func showLogs(_ sender: Any?) {
        Paths.ensurePrivateDirectory(Paths.logs)
        NSWorkspace.shared.open(Paths.logs)
    }

    @objc private func showInstancesFile(_ sender: Any?) {
        NSWorkspace.shared.activateFileViewerSelecting([Paths.instancesFile])
    }

    @objc private func installHelper(_ sender: Any?) {
        guard let bundled = HelperInstaller.bundled else { return }
        let alert = NSAlert()
        let current = HelperInstaller.version(of: HelperInstaller.installed)
        alert.messageText = "„DSH Computer Use“ installieren?"
        alert.informativeText = "Kopiert den mitgelieferten Helfer \(HelperInstaller.version(of: bundled) ?? "") nach ~/Applications"
            + (current.map { " und ersetzt Version \($0)." } ?? ".")
            + " Bedienungshilfen und Bildschirmaufnahme müssen danach eventuell neu erlaubt werden."
        alert.addButton(withTitle: "Installieren")
        alert.addButton(withTitle: "Abbrechen")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        if let error = HelperInstaller.install(from: bundled) {
            NSAlert(error: error).runModal()
        }
    }

    // MARK: - Menu

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(installHelper(_:)): return HelperInstaller.bundled != nil
        case #selector(reloadPage(_:)), #selector(zoomIn(_:)), #selector(zoomOut(_:)), #selector(zoomReset(_:)),
             #selector(openProject(_:)), #selector(pageCommand(_:)):
            return main?.currentWebView != nil
        default: return true
        }
    }

    private func instancesChanged() {
        rebuildInstanceMenu()
        instancesWindow?.reload()
    }

    private func rebuildInstanceMenu() {
        instanceMenu.removeAllItems()
        for (index, instance) in manager.instances.enumerated() {
            // ⌃1 … ⌃9 like Codex's areas; ⌘1 … ⌘9 belong to the chats.
            let item = NSMenuItem(title: instance.name, action: #selector(switchInstance(_:)),
                                  keyEquivalent: index < 9 ? String(index + 1) : "")
            item.keyEquivalentModifierMask = .control
            item.representedObject = instance.id
            item.state = instance.id == manager.currentID ? .on : .off
            item.target = self
            let status = manager.status(instance.id)
            if status == .error || status == .starting {
                item.badge = NSMenuItemBadge(string: status == .error ? "Fehler" : "verbindet")
            }
            instanceMenu.addItem(item)
        }
        instanceMenu.addItem(.separator())
        instanceMenu.addItem(item("Neu verbinden", #selector(reconnect(_:)), "r", [.command, .shift]))
        instanceMenu.addItem(.separator())
        instanceMenu.addItem(item("Instanz hinzufügen …", #selector(addInstance(_:)), ""))
        instanceMenu.addItem(item("Instanzen bearbeiten …", #selector(editInstances(_:)), ",", [.command, .option]))
    }

    private func item(_ title: String, _ action: Selector?, _ key: String,
                      _ modifiers: NSEvent.ModifierFlags = .command, target: AnyObject? = nil) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.keyEquivalentModifierMask = modifiers
        if let target { item.target = target } else if let action, responds(to: action) { item.target = self }
        return item
    }

    /// An item that runs a command of the page (see `pageCommand`).
    private func command(_ title: String, _ name: String, _ key: String,
                         _ modifiers: NSEvent.ModifierFlags = .command) -> NSMenuItem {
        let entry = item(title, #selector(pageCommand(_:)), key, modifiers)
        entry.representedObject = name
        return entry
    }

    private func buildMenu() -> NSMenu {
        let menu = NSMenu()
        let name = "Aether"

        let app = NSMenu(title: name)
        app.addItem(item("Über \(name)", #selector(NSApplication.orderFrontStandardAboutPanel(_:)), "", target: NSApp))
        app.addItem(.separator())
        app.addItem(item("Einstellungen …", #selector(openSettings(_:)), ","))
        app.addItem(item("Instanzen bearbeiten …", #selector(editInstances(_:)), ",", [.command, .option]))
        app.addItem(.separator())
        let services = NSMenu(title: "Dienste")
        let servicesItem = NSMenuItem(title: "Dienste", action: nil, keyEquivalent: "")
        servicesItem.submenu = services
        NSApp.servicesMenu = services
        app.addItem(servicesItem)
        app.addItem(.separator())
        app.addItem(item("\(name) ausblenden", #selector(NSApplication.hide(_:)), "h", target: NSApp))
        app.addItem(item("Andere ausblenden", #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option], target: NSApp))
        app.addItem(item("Alle einblenden", #selector(NSApplication.unhideAllApplications(_:)), "", target: NSApp))
        app.addItem(.separator())
        app.addItem(item("\(name) beenden", #selector(NSApplication.terminate(_:)), "q", target: NSApp))

        let file = NSMenu(title: "Ablage")
        file.addItem(item("Neuer Chat", #selector(newChat(_:)), "n"))
        file.addItem(item("Projekt öffnen …", #selector(openProject(_:)), "o"))
        file.addItem(item("Ordner übertragen …", #selector(transferFolder(_:)), ""))
        file.addItem(.separator())
        file.addItem(item("Fenster schließen", #selector(NSWindow.performClose(_:)), "w"))

        let edit = NSMenu(title: "Bearbeiten")
        edit.addItem(item("Widerrufen", Selector(("undo:")), "z"))
        edit.addItem(item("Wiederholen", Selector(("redo:")), "z", [.command, .shift]))
        edit.addItem(.separator())
        edit.addItem(item("Ausschneiden", #selector(NSText.cut(_:)), "x"))
        edit.addItem(item("Kopieren", #selector(NSText.copy(_:)), "c"))
        edit.addItem(item("Einsetzen", #selector(NSText.paste(_:)), "v"))
        edit.addItem(item("Einsetzen und Stil anpassen", #selector(NSTextView.pasteAsPlainText(_:)), "v", [.command, .option, .shift]))
        edit.addItem(item("Alles auswählen", #selector(NSText.selectAll(_:)), "a"))
        edit.addItem(.separator())
        edit.addItem(command("Im Chat suchen …", "find", "f"))

        let view = NSMenu(title: "Darstellung")
        view.addItem(item("Neu laden", #selector(reloadPage(_:)), "r"))
        view.addItem(.separator())
        view.addItem(item("Originalgröße", #selector(zoomReset(_:)), "0"))
        view.addItem(item("Vergrößern", #selector(zoomIn(_:)), "+"))
        let zoomInAlias = item("Vergrößern", #selector(zoomIn(_:)), "=")
        zoomInAlias.isHidden = true
        zoomInAlias.allowsKeyEquivalentWhenHidden = true
        view.addItem(zoomInAlias)
        view.addItem(item("Verkleinern", #selector(zoomOut(_:)), "-"))
        view.addItem(.separator())
        view.addItem(item("Vollbild", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]))

        // Codex's chat shortcuts; the page handles the keys itself and runs a
        // command only once when the menu sees the same key.
        let chat = NSMenu(title: "Chat")
        chat.addItem(command("Umbenennen", "rename-chat", "r", [.command, .option]))
        chat.addItem(command("Anpinnen oder lösen", "pin-chat", "p", [.command, .option]))
        chat.addItem(command("Archivieren", "archive-chat", "a", [.command, .shift]))
        chat.addItem(.separator())
        chat.addItem(command("Vorheriger Chat", "previous-chat", "[", [.command, .shift]))
        chat.addItem(command("Nächster Chat", "next-chat", "]", [.command, .shift]))
        chat.addItem(command("Nächster Chat, der dich braucht", "next-attention", "a", [.command, .option]))
        chat.addItem(.separator())
        // ⇧Esc in the page; no key here, so typing in a field never triggers it.
        chat.addItem(command("Alle als gelesen markieren", "mark-all-read", ""))

        rebuildInstanceMenu()

        let window = NSMenu(title: "Fenster")
        window.addItem(item("Im Dock ablegen", #selector(NSWindow.performMiniaturize(_:)), "m"))
        window.addItem(item("Zoomen", #selector(NSWindow.performZoom(_:)), ""))
        window.addItem(.separator())
        window.addItem(item("Aether", #selector(showMainWindow(_:)), "0", [.command, .shift]))
        window.addItem(item("Alle nach vorne bringen", #selector(NSApplication.arrangeInFront(_:)), "", target: NSApp))
        NSApp.windowsMenu = window

        let help = NSMenu(title: "Hilfe")
        help.addItem(command("Tastenkürzel", "shortcuts", "/"))
        help.addItem(.separator())
        help.addItem(item("Protokolle anzeigen", #selector(showLogs(_:)), ""))
        help.addItem(item("instances.json im Finder zeigen", #selector(showInstancesFile(_:)), ""))
        help.addItem(item("Computer-Use-Helfer installieren …", #selector(installHelper(_:)), ""))
        help.addItem(.separator())
        help.addItem(item("Mitteilungen testen", #selector(testNotifications(_:)), ""))
        NSApp.helpMenu = help

        for submenu in [app, file, edit, view, chat, instanceMenu, window, help] {
            let top = NSMenuItem(title: submenu.title, action: nil, keyEquivalent: "")
            top.submenu = submenu
            menu.addItem(top)
        }
        return menu
    }
}

/// Before the app stops its local servers: which chats there still run a
/// turn (`GET /api/sessions`, status `running`/`waiting-approval`) and which
/// have background jobs alive (`job/registered` since the server started
/// without `job/settled`, from the logs of chats changed since then). Every
/// request has a short limit; what does not answer in time counts as idle,
/// so quitting never hangs on a server.
enum QuitCheck {
    struct Work: Sendable {
        /// "„Title“ (instance)" of chats with a running turn.
        var chats: [String] = []
        /// "„Title“ (instance)" and the number of live jobs.
        var jobs: [(String, Int)] = []

        var jobCount: Int { jobs.reduce(0) { $0 + $1.1 } }
        var isEmpty: Bool { chats.isEmpty && jobs.isEmpty }

        mutating func merge(_ other: Work) {
            chats += other.chats
            jobs += other.jobs
        }

        var headline: String {
            let chatText = chats.count == 1 ? "1 Chat arbeitet noch" : "\(chats.count) Chats arbeiten noch"
            switch (chats.isEmpty, jobs.isEmpty) {
            case (false, true): return chatText
            case (true, false): return jobCount == 1 ? "1 Hintergrundjob läuft noch" : "\(jobCount) Hintergrundjobs laufen noch"
            default: return chatText + ", " + (jobCount == 1 ? "1 Hintergrundjob läuft" : "\(jobCount) Hintergrundjobs laufen")
            }
        }

        var details: String {
            var lines = chats.prefix(6).map { "• \($0)" }
            if chats.count > 6 { lines.append("• … und \(chats.count - 6) weitere") }
            lines += jobs.prefix(6).map { "• \($0.0): \($0.1 == 1 ? "1 Hintergrundjob" : "\($0.1) Hintergrundjobs")" }
            if jobs.count > 6 { lines.append("• … und \(jobs.count - 6) weitere Chats mit Hintergrundjobs") }
            lines.append("")
            lines.append("Beim Beenden brechen laufende Antworten ab und Hintergrundjobs werden beendet.")
            return lines.joined(separator: "\n")
        }
    }

    /// Limit of each request; the list and the log reads run one after the other.
    private static let limit: TimeInterval = 1.5
    /// Logs read per server at most (the chats changed last).
    private static let maxLogs = 20

    private static let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = limit
        config.timeoutIntervalForResource = limit
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.connectionProxyDictionary = [:]
        return URLSession(configuration: config)
    }()

    /// The busy work of every connected local instance (only those stop on quit).
    @MainActor
    static func run(_ manager: InstanceManager) async -> Work {
        let targets: [(String, InstanceAPI, Date)] = manager.instances.compactMap { instance in
            guard instance.kind == .local,
                  let connection = manager.existingConnection(instance.id) as? LocalConnection,
                  let api = InstanceAPI(connection) else { return nil }
            return (instance.name, api, connection.startedAt)
        }
        guard !targets.isEmpty else { return Work() }
        return await withTaskGroup(of: Work.self) { group in
            for (name, api, since) in targets {
                group.addTask { await check(name, api, since: since) }
            }
            var total = Work()
            for await work in group { total.merge(work) }
            return total
        }
    }

    private static func json(_ api: InstanceAPI, _ path: String) async -> Any? {
        guard let (data, response) = try? await session.data(for: api.request("GET", path)),
              (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        return try? JSONSerialization.jsonObject(with: data)
    }

    private static func check(_ instance: String, _ api: InstanceAPI, since: Date) async -> Work {
        guard let rows = await json(api, "/api/sessions") as? [[String: Any]] else { return Work() }
        let sinceMs = since.timeIntervalSince1970 * 1000
        let number = { (v: Any?) in (v as? NSNumber)?.doubleValue ?? 0 }
        let label = { (row: [String: Any]) -> String in
            let title = (row["title"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "Chat ohne Titel"
            return "„\(title)“ (\(instance))"
        }
        var work = Work()
        var candidates: [(id: String, name: String, updated: Double)] = []
        for row in rows {
            let status = row["status"] as? String ?? ""
            if status == "running" || status == "waiting-approval" {
                work.chats.append(label(row))
            } else if let id = row["id"] as? String, number(row["updatedAt"]) >= sinceMs {
                // A job registration is an event: only chats changed since the start can own one.
                candidates.append((id, label(row), number(row["updatedAt"])))
            }
        }
        let probes = candidates.sorted { $0.updated > $1.updated }.prefix(maxLogs)
        work.jobs = await withTaskGroup(of: (String, Int).self) { group in
            for probe in probes {
                group.addTask {
                    let id = probe.id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? probe.id
                    let body = await json(api, "/api/sessions/\(id)") as? [String: Any]
                    return (probe.name, liveJobs(body?["events"] as? [[String: Any]] ?? [], sinceMs: sinceMs))
                }
            }
            var found: [(String, Int)] = []
            for await (name, count) in group where count > 0 { found.append((name, count)) }
            return found
        }
        return work
    }

    /// Jobs this server process registered in a log and did not settle yet.
    private static func liveJobs(_ events: [[String: Any]], sinceMs: Double) -> Int {
        var live = Set<String>()
        for event in events {
            guard let type = event["type"] as? String,
                  let id = (event["data"] as? [String: Any])?["jobId"] as? String else { continue }
            if type == "job/registered", ((event["time"] as? NSNumber)?.doubleValue ?? 0) >= sinceMs {
                live.insert(id)
            } else if type == "job/settled" {
                live.remove(id)
            }
        }
        return live.count
    }
}
