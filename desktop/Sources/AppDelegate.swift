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
        var replied = false
        let reply = {
            guard !replied else { return }
            replied = true
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        manager.stopAll { reply() }
        // Never hang on quit: local servers get SIGKILL after 6 s anyway.
        DispatchQueue.main.asyncAfter(deadline: .now() + 8) { reply() }
        return .terminateLater
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
             #selector(openProject(_:)):
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

    private func buildMenu() -> NSMenu {
        let menu = NSMenu()
        let name = "dsh-codex"

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
        edit.addItem(item("Suchen …", #selector(NSTextView.performFindPanelAction(_:)), "f"))

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

        rebuildInstanceMenu()

        let window = NSMenu(title: "Fenster")
        window.addItem(item("Im Dock ablegen", #selector(NSWindow.performMiniaturize(_:)), "m"))
        window.addItem(item("Zoomen", #selector(NSWindow.performZoom(_:)), ""))
        window.addItem(.separator())
        window.addItem(item("dsh-codex", #selector(showMainWindow(_:)), "0", [.command, .shift]))
        window.addItem(item("Alle nach vorne bringen", #selector(NSApplication.arrangeInFront(_:)), "", target: NSApp))
        NSApp.windowsMenu = window

        let help = NSMenu(title: "Hilfe")
        help.addItem(item("Protokolle anzeigen", #selector(showLogs(_:)), ""))
        help.addItem(item("instances.json im Finder zeigen", #selector(showInstancesFile(_:)), ""))
        help.addItem(item("Computer-Use-Helfer installieren …", #selector(installHelper(_:)), ""))
        help.addItem(.separator())
        help.addItem(item("Mitteilungen testen", #selector(testNotifications(_:)), ""))
        NSApp.helpMenu = help

        for submenu in [app, file, edit, view, instanceMenu, window, help] {
            let top = NSMenuItem(title: submenu.title, action: nil, keyEquivalent: "")
            top.submenu = submenu
            menu.addItem(top)
        }
        return menu
    }
}
