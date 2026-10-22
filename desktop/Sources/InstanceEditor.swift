import AppKit

/// Sheet "Instanz hinzufügen / bearbeiten".
@MainActor
final class InstanceEditor: NSObject {
    private let panel: NSPanel
    private let original: Instance?
    private let others: [Instance]
    private var completion: ((Instance, String??) -> Void)?

    private let name = NSTextField()
    private let kind = NSPopUpButton()
    private let atLaunch = NSButton(checkboxWithTitle: "Beim Start verbinden", target: nil, action: nil)
    // local
    private let binary = NSTextField()
    private let workingDirectory = NSTextField()
    private let dataDirectory = NSTextField()
    // ssh
    private let sshConfig = NSTextField()
    private let host = NSTextField()
    private let remotePort = NSTextField()
    private let localPort = NSTextField()
    private let tokenCommand = NSTextView()
    // url
    private let url = NSTextField()
    private let token = NSSecureTextField()
    private let error = NSTextField(labelWithString: "")

    private var groups: [InstanceKind: NSView] = [:]

    init(instance: Instance?, others: [Instance]) {
        original = instance
        self.others = others
        panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 560, height: 300),
                        styleMask: [.titled], backing: .buffered, defer: true)
        super.init()
        build()
        fill(instance ?? Instance(id: Instance.newID(), name: "", kind: .ssh, remotePort: 3080))
    }

    func begin(on window: NSWindow?, completion: @escaping (Instance, String??) -> Void) {
        self.completion = completion
        if let window {
            window.beginSheet(panel)
        } else {
            panel.center()
            NSApp.runModal(for: panel)
        }
    }

    // MARK: - Layout

    private func label(_ text: String) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.alignment = .right
        return label
    }

    private func hint(_ text: String) -> NSTextField {
        let hint = NSTextField(wrappingLabelWithString: text)
        hint.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        hint.textColor = .secondaryLabelColor
        return hint
    }

    private func browseRow(_ field: NSTextField, directory: Bool, placeholder: String) -> NSView {
        field.placeholderString = placeholder
        let button = NSButton(title: "Auswählen …", target: self, action: #selector(browse(_:)))
        button.bezelStyle = .rounded
        button.identifier = NSUserInterfaceItemIdentifier(directory ? "dir" : "file")
        objc_setAssociatedObject(button, &InstanceEditor.fieldKey, field, .OBJC_ASSOCIATION_RETAIN)
        let row = NSStackView(views: [field, button])
        row.spacing = 8
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return row
    }

    private static var fieldKey = 0

    private func grid(_ rows: [[NSView]]) -> NSGridView {
        let grid = NSGridView(views: rows)
        grid.rowSpacing = 8
        grid.columnSpacing = 10
        grid.column(at: 0).xPlacement = .trailing
        grid.column(at: 0).width = 130
        grid.column(at: 1).width = 380
        grid.rowAlignment = .firstBaseline
        return grid
    }

    private func build() {
        panel.title = "Instanz"
        for kind in InstanceKind.allCases {
            self.kind.addItem(withTitle: kind.label)
            self.kind.lastItem?.representedObject = kind.rawValue
        }
        kind.target = self
        kind.action = #selector(kindChanged)
        name.placeholderString = "z. B. VPS 2"

        let common = grid([
            [label("Name:"), name],
            [label("Typ:"), kind],
            [NSGridCell.emptyContentView, atLaunch],
            [NSGridCell.emptyContentView, hint("Verbindet die Instanz schon beim App-Start im Hintergrund, damit der Wechsel sofort da ist.")],
        ])

        let local = grid([
            [label("Programm:"), browseRow(binary, directory: false, placeholder: "Mitgeliefertes dsh-codex")],
            [label("Arbeitsordner:"), browseRow(workingDirectory, directory: true, placeholder: "Benutzerordner")],
            [label("Datenordner:"), browseRow(dataDirectory, directory: true, placeholder: "~/.dsh-codex (DSHX_HOME)")],
            [NSGridCell.emptyContentView, hint("Startet „dsh-codex web --no-open --port 0“ auf diesem Mac. Leere Felder verwenden die Vorgaben.")],
        ])

        tokenCommand.isRichText = false
        tokenCommand.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        tokenCommand.isAutomaticQuoteSubstitutionEnabled = false
        tokenCommand.isAutomaticDashSubstitutionEnabled = false
        tokenCommand.isAutomaticTextReplacementEnabled = false
        tokenCommand.isAutomaticSpellingCorrectionEnabled = false
        tokenCommand.textContainerInset = NSSize(width: 4, height: 4)
        let scroll = NSScrollView()
        scroll.documentView = tokenCommand
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        tokenCommand.autoresizingMask = [.width]
        tokenCommand.isVerticallyResizable = true
        tokenCommand.minSize = NSSize(width: 0, height: 60)
        tokenCommand.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: .greatestFiniteMagnitude)
        tokenCommand.textContainer?.widthTracksTextView = true
        scroll.heightAnchor.constraint(equalToConstant: 64).isActive = true
        scroll.widthAnchor.constraint(equalToConstant: 380).isActive = true
        remotePort.placeholderString = "3080"
        localPort.placeholderString = "3082"
        host.placeholderString = "Host-Alias aus der SSH-Konfiguration"
        let ports = NSStackView(views: [remotePort, label("Lokaler Port:"), localPort])
        ports.spacing = 8
        remotePort.widthAnchor.constraint(equalToConstant: 80).isActive = true
        localPort.widthAnchor.constraint(equalToConstant: 80).isActive = true
        let ssh = grid([
            [label("SSH-Konfiguration:"), browseRow(sshConfig, directory: false, placeholder: "~/.ssh/config")],
            [label("Host:"), host],
            [label("Remote-Port:"), ports],
            [label("Token-Befehl:"), scroll],
            [NSGridCell.emptyContentView, hint("Läuft per SSH auf dem Server und gibt den Startlink (…?token=…) oder das Token aus. Der Tunnel nutzt /usr/bin/ssh -N -L 127.0.0.1:Lokal → 127.0.0.1:Remote; Hostschlüssel werden wie konfiguriert geprüft.")],
        ])

        url.placeholderString = "http://127.0.0.1:3080/"
        token.placeholderString = original?.kind == .url ? "unverändert" : "Zugriffstoken"
        let web = grid([
            [label("URL:"), url],
            [label("Token:"), token],
            [NSGridCell.emptyContentView, hint("Das Token wird im Schlüsselbund gespeichert, nicht in instances.json.")],
        ])
        groups = [.local: local, .ssh: ssh, .url: web]

        error.textColor = .systemRed
        error.isHidden = true
        let cancel = NSButton(title: "Abbrechen", target: self, action: #selector(cancelClicked))
        cancel.keyEquivalent = "\u{1b}"
        let save = NSButton(title: original == nil ? "Hinzufügen" : "Sichern", target: self, action: #selector(saveClicked))
        save.keyEquivalent = "\r"
        let spacer = NSView()
        let buttons = NSStackView(views: [error, spacer, cancel, save])
        buttons.spacing = 10
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)

        let heading = NSTextField(labelWithString: original == nil ? "Instanz hinzufügen" : "Instanz bearbeiten")
        heading.font = .systemFont(ofSize: 15, weight: .semibold)
        let stack = NSStackView(views: [heading, common, local, ssh, web, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)
        stack.translatesAutoresizingMaskIntoConstraints = false
        buttons.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40).isActive = true
        let content = NSView()
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])
        panel.contentView = content
    }

    private func fill(_ instance: Instance) {
        name.stringValue = instance.name
        kind.selectItem(at: InstanceKind.allCases.firstIndex(of: instance.kind) ?? 0)
        atLaunch.state = instance.connectsAtLaunch ? .on : .off
        binary.stringValue = instance.binary ?? ""
        workingDirectory.stringValue = instance.workingDirectory ?? ""
        dataDirectory.stringValue = instance.dataDirectory ?? ""
        sshConfig.stringValue = instance.sshConfig ?? ""
        host.stringValue = instance.host ?? ""
        remotePort.stringValue = instance.remotePort.map(String.init) ?? ""
        localPort.stringValue = instance.localPort.map(String.init) ?? ""
        tokenCommand.string = instance.tokenCommand ?? Defaults.vpsTokenCommand
        url.stringValue = instance.url ?? ""
        kindChanged()
    }

    private var selectedKind: InstanceKind {
        (kind.selectedItem?.representedObject as? String).flatMap(InstanceKind.init(rawValue:)) ?? .ssh
    }

    @objc private func kindChanged() {
        for (kind, view) in groups { view.isHidden = kind != selectedKind }
        panel.layoutIfNeeded()
        panel.setContentSize(panel.contentView?.fittingSize ?? panel.frame.size)
    }

    @objc private func browse(_ sender: NSButton) {
        guard let field = objc_getAssociatedObject(sender, &InstanceEditor.fieldKey) as? NSTextField else { return }
        let open = NSOpenPanel()
        let directory = sender.identifier?.rawValue == "dir"
        open.canChooseDirectories = directory
        open.canChooseFiles = !directory
        open.showsHiddenFiles = true
        open.beginSheetModal(for: panel) { response in
            if response == .OK, let url = open.url { field.stringValue = url.path }
        }
    }

    private func trimmed(_ field: NSTextField) -> String? {
        let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }

    private func port(_ field: NSTextField, _ fallback: Int) -> Int? {
        guard let text = trimmed(field) else { return fallback }
        guard let value = Int(text), (1...65535).contains(value) else { return nil }
        return value
    }

    private func fail(_ message: String) {
        error.stringValue = message
        error.isHidden = false
        NSSound.beep()
    }

    @objc private func saveClicked() {
        guard let name = trimmed(name) else { return fail("Bitte einen Namen angeben.") }
        var instance = Instance(id: original?.id ?? Instance.newID(), name: name, kind: selectedKind)
        instance.connectAtLaunch = atLaunch.state == .on ? nil : false
        var newToken: String?? = nil
        switch selectedKind {
        case .local:
            instance.binary = trimmed(binary)
            instance.workingDirectory = trimmed(workingDirectory)
            instance.dataDirectory = trimmed(dataDirectory)
        case .ssh:
            guard let host = trimmed(host) else { return fail("Bitte den Host-Alias angeben.") }
            guard let remote = port(remotePort, 3080), let local = port(localPort, 3082) else {
                return fail("Ports müssen zwischen 1 und 65535 liegen.")
            }
            if let clash = others.first(where: { $0.kind == .ssh && $0.localPort == local }) {
                return fail("Lokaler Port \(local) wird schon von „\(clash.name)“ verwendet.")
            }
            let command = tokenCommand.string.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !command.isEmpty else { return fail("Bitte einen Token-Befehl angeben.") }
            instance.sshConfig = trimmed(sshConfig)
            instance.host = host
            instance.remotePort = remote
            instance.localPort = local
            instance.tokenCommand = command
        case .url:
            guard let raw = trimmed(url), let parsed = URL(string: raw),
                  ["http", "https"].contains(parsed.scheme?.lowercased() ?? ""), parsed.host != nil
            else { return fail("Bitte eine http(s)-URL angeben.") }
            instance.url = raw
            if let value = trimmed(token) {
                newToken = .some(value)
            } else if original?.kind != .url {
                newToken = .some(nil)
            }
        }
        close()
        completion?(instance, newToken)
    }

    @objc private func cancelClicked() { close() }

    private func close() {
        if let parent = panel.sheetParent { parent.endSheet(panel) } else { NSApp.stopModal(); panel.orderOut(nil) }
    }
}

/// Window "Instanzen": list, order, add, edit, remove.
@MainActor
final class InstancesWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    private let manager: InstanceManager
    private let table = NSTableView()
    private let remove = NSButton()
    private let edit = NSButton()
    private let up = NSButton()
    private let down = NSButton()
    private let editorPopup = NSPopUpButton()
    private var editor: InstanceEditor?

    init(manager: InstanceManager) {
        self.manager = manager
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 360),
                              styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: true)
        window.title = "Instanzen"
        window.minSize = NSSize(width: 460, height: 280)
        window.setFrameAutosaveName("dsh-codex.instances")
        super.init(window: window)
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    private func column(_ id: String, _ title: String, _ width: CGFloat) -> NSTableColumn {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(id))
        column.title = title
        column.width = width
        return column
    }

    private func build() {
        table.addTableColumn(column("key", "", 34))
        table.addTableColumn(column("name", "Name", 180))
        table.addTableColumn(column("kind", "Typ", 110))
        table.addTableColumn(column("status", "Status", 120))
        table.dataSource = self
        table.delegate = self
        table.usesAlternatingRowBackgroundColors = true
        table.doubleAction = #selector(editClicked)
        table.target = self
        table.style = .inset
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        let add = NSButton(image: NSImage(systemSymbolName: "plus", accessibilityDescription: "Hinzufügen")!,
                           target: self, action: #selector(addClicked))
        remove.image = NSImage(systemSymbolName: "minus", accessibilityDescription: "Entfernen")
        remove.target = self
        remove.action = #selector(removeClicked)
        up.image = NSImage(systemSymbolName: "chevron.up", accessibilityDescription: "Nach oben")
        up.target = self
        up.action = #selector(upClicked)
        down.image = NSImage(systemSymbolName: "chevron.down", accessibilityDescription: "Nach unten")
        down.target = self
        down.action = #selector(downClicked)
        for button in [add, remove, up, down] { button.bezelStyle = .smallSquare }
        edit.title = "Bearbeiten …"
        edit.bezelStyle = .rounded
        edit.target = self
        edit.action = #selector(editClicked)
        let open = NSButton(title: "Anzeigen", target: self, action: #selector(openClicked))
        open.bezelStyle = .rounded
        open.keyEquivalent = "\r"

        editorPopup.target = self
        editorPopup.action = #selector(editorChanged)
        fillEditors()

        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let bar = NSStackView(views: [add, remove, up, down, spacer, edit, open])
        bar.spacing = 6
        let editorRow = NSStackView(views: [NSTextField(labelWithString: "Editor für „In Editor öffnen“:"), editorPopup])
        editorRow.spacing = 8
        let stack = NSStackView(views: [scroll, bar, editorRow])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            bar.widthAnchor.constraint(equalTo: scroll.widthAnchor),
        ])
        window?.contentView = content
        updateButtons()
    }

    private func fillEditors() {
        editorPopup.removeAllItems()
        editorPopup.addItem(withTitle: "Automatisch")
        editorPopup.lastItem?.representedObject = ""
        for id in Editor.candidates {
            guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: id) else { continue }
            editorPopup.addItem(withTitle: FileManager.default.displayName(atPath: url.path))
            editorPopup.lastItem?.representedObject = id
        }
        let current = manager.file.editor ?? ""
        if let item = editorPopup.itemArray.first(where: { $0.representedObject as? String == current }) {
            editorPopup.select(item)
        }
    }

    func reload() {
        let selected = selectedInstance?.id
        table.reloadData()
        if let selected, let row = manager.instances.firstIndex(where: { $0.id == selected }) {
            table.selectRowIndexes([row], byExtendingSelection: false)
        }
        updateButtons()
    }

    private var selectedInstance: Instance? {
        manager.instances.indices.contains(table.selectedRow) ? manager.instances[table.selectedRow] : nil
    }

    private func updateButtons() {
        let row = table.selectedRow
        let has = selectedInstance != nil
        remove.isEnabled = has && manager.instances.count > 1
        edit.isEnabled = has
        up.isEnabled = has && row > 0
        down.isEnabled = has && row < manager.instances.count - 1
    }

    func numberOfRows(in tableView: NSTableView) -> Int { manager.instances.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let instance = manager.instances[row]
        let text: String
        switch tableColumn?.identifier.rawValue {
        case "key": text = row < 9 ? "⌃\(row + 1)" : ""
        case "name": text = instance.name + (instance.id == manager.currentID ? "  ✓" : "")
        case "kind": text = instance.kind.label
        default:
            switch manager.status(instance.id) {
            case .idle: text = "–"
            case .starting: text = "verbindet …"
            case .ready: text = "verbunden"
            case .error: text = "Fehler"
            }
        }
        let field = NSTextField(labelWithString: text)
        field.lineBreakMode = .byTruncatingTail
        if tableColumn?.identifier.rawValue == "key" { field.textColor = .tertiaryLabelColor }
        return field
    }

    func tableViewSelectionDidChange(_ notification: Notification) { updateButtons() }

    /// Opens the add sheet on `window` (this window by default).
    func presentEditor(for instance: Instance?, on window: NSWindow?) {
        let editor = InstanceEditor(instance: instance, others: manager.instances.filter { $0.id != instance?.id })
        self.editor = editor
        editor.begin(on: window ?? self.window) { [weak self] saved, token in
            guard let self else { return }
            let isNew = instance == nil
            self.manager.save(saved, token: token)
            self.editor = nil
            self.reload()
            if isNew { self.manager.activate(saved.id) }
        }
    }

    @objc private func addClicked() { presentEditor(for: nil, on: window) }
    @objc private func editClicked() {
        guard let instance = selectedInstance else { return }
        presentEditor(for: instance, on: window)
    }

    @objc private func openClicked() {
        guard let instance = selectedInstance else { return }
        manager.activate(instance.id)
        NSApp.sendAction(#selector(AppDelegate.showMainWindow(_:)), to: nil, from: self)
    }

    @objc private func removeClicked() {
        guard let instance = selectedInstance, let window else { return }
        let alert = NSAlert()
        alert.messageText = "„\(instance.name)“ entfernen?"
        alert.informativeText = "Die Verbindung wird getrennt. Daten auf dem Server bleiben unverändert."
        alert.addButton(withTitle: "Entfernen")
        alert.addButton(withTitle: "Abbrechen")
        alert.buttons.first?.hasDestructiveAction = true
        alert.beginSheetModal(for: window) { response in
            guard response == .alertFirstButtonReturn else { return }
            self.manager.remove(instance.id)
            self.reload()
        }
    }

    @objc private func upClicked() { move(-1) }
    @objc private func downClicked() { move(1) }

    private func move(_ offset: Int) {
        guard let instance = selectedInstance else { return }
        manager.move(instance.id, by: offset)
        reload()
    }

    @objc private func editorChanged() {
        let value = editorPopup.selectedItem?.representedObject as? String ?? ""
        manager.setEditor(value.isEmpty ? nil : value)
    }
}
