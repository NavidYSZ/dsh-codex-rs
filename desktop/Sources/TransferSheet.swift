import AppKit
import Foundation

// "Ordner übertragen …": copies, moves or syncs a project folder from one
// instance to another (docs/aether/TRANSFER.md). The instances cannot reach
// each other, the app sees both: it exports from the source into a 0600 file
// in its cache, uploads that file to the target, verifies the result against
// the target's manifest and only then removes the source of a move.

/// Excludes preselected in the sheet (`.git` and hidden files are included).
private let defaultExcludes = [
    "node_modules", "target", ".venv", "venv", "__pycache__", "DerivedData", ".build", ".cache", ".DS_Store",
]

/// The token-protected HTTP API of one connected instance.
struct InstanceAPI: Sendable {
    let origin: URL
    let token: String?

    /// The API behind a ready connection; its page URL carries the token.
    @MainActor
    init?(_ connection: Connection) {
        guard connection.status == .ready, let page = connection.pageURL, let origin = Net.origin(of: page)
        else { return nil }
        self.origin = origin
        token = URLComponents(url: page, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == "token" })?.value
    }

    private static let queryAllowed = CharacterSet(
        charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~/")

    func request(_ method: String, _ path: String, query: [(String, String)] = [], json: Any? = nil) -> URLRequest {
        var parts = URLComponents(url: origin, resolvingAgainstBaseURL: false) ?? URLComponents()
        parts.path = path
        if !query.isEmpty {
            // Strict encoding: `+`, `&` and `=` in a path stay data.
            parts.percentEncodedQuery = query.map {
                "\($0.0)=\($0.1.addingPercentEncoding(withAllowedCharacters: Self.queryAllowed) ?? "")"
            }.joined(separator: "&")
        }
        var request = URLRequest(url: parts.url ?? origin)
        request.httpMethod = method
        request.cachePolicy = .reloadIgnoringLocalCacheData
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let json, let body = try? JSONSerialization.data(withJSONObject: json) {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        return request
    }

    private static let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        // A manifest of a big tree takes a while.
        config.timeoutIntervalForRequest = 600
        config.timeoutIntervalForResource = 3600
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.connectionProxyDictionary = [:]
        return URLSession(configuration: config)
    }()

    /// A JSON call; error answers become `TransferError.server`.
    func call(_ method: String, _ path: String, json: Any? = nil) async throws -> Any {
        let (data, response) = try await Self.session.data(for: request(method, path, json: json))
        return try Self.decode(data, response)
    }

    static func decode(_ data: Data, _ response: URLResponse) throws -> Any {
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        let value = try? JSONSerialization.jsonObject(with: data)
        guard (200..<300).contains(status) else {
            let body = value as? [String: Any]
            // Older servers answer unknown API routes with the app shell or 405.
            if body?["error"] == nil, status == 404 || status == 405 { throw TransferError.unsupported }
            throw TransferError.server(
                status: status, code: body?["code"] as? String ?? "",
                message: body?["error"] as? String ?? "HTTP \(status)")
        }
        guard let value else { throw TransferError.unsupported }
        return value
    }
}

enum TransferError: LocalizedError {
    case server(status: Int, code: String, message: String)
    case unsupported
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .server(_, _, let message): return message
        case .unsupported: return "Die Instanz kennt die Ordnerübertragung noch nicht (Aether dort aktualisieren)."
        case .failed(let message): return message
        }
    }
}

/// `POST /api/transfer/manifest`: files, symlinks and empty folders.
struct Manifest {
    struct Entry {
        let path: String
        let kind: String
        let size: Int64
        let mtime: Int64
        let target: String?
    }

    let root: String
    let totalBytes: Int64
    let excluded: Int
    let entries: [Entry]
    let byPath: [String: Entry]

    init(_ value: Any) throws {
        guard let body = value as? [String: Any], let root = body["root"] as? String,
              let files = body["files"] as? [[String: Any]]
        else { throw TransferError.failed("Unerwartete Antwort auf die Dateiliste.") }
        let number = { (v: Any?) in (v as? NSNumber)?.int64Value ?? 0 }
        self.root = root
        totalBytes = number(body["totalBytes"])
        excluded = Int(number(body["excluded"]))
        entries = files.compactMap { f in
            guard let path = f["path"] as? String, let kind = f["kind"] as? String else { return nil }
            return Entry(path: path, kind: kind, size: number(f["size"]), mtime: number(f["mtime"]),
                         target: f["target"] as? String)
        }
        byPath = Dictionary(entries.map { ($0.path, $0) }, uniquingKeysWith: { a, _ in a })
    }

    /// Same content as far as a sync can tell: kind, size, mtime (±2 s), link target.
    static func same(_ a: Entry, _ b: Entry) -> Bool {
        guard a.kind == b.kind else { return false }
        switch a.kind {
        case "file": return a.size == b.size && abs(a.mtime - b.mtime) <= 2000
        case "symlink": return a.target == b.target
        default: return true
        }
    }

    /// Folders that exist here: every parent of an entry and the empty ones.
    var folders: Set<String> {
        var result = Set<String>()
        for entry in entries {
            if entry.kind == "dir" { result.insert(entry.path) }
            var prefix = ""
            for part in entry.path.split(separator: "/").dropLast() {
                prefix = prefix.isEmpty ? String(part) : prefix + "/" + part
                result.insert(prefix)
            }
        }
        return result
    }
}

/// One streaming request with byte progress: the export into a file or the
/// upload of a file. Cancelling the Swift task cancels the request.
final class StreamingRequest: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let queue = OperationQueue()
    private var sink: FileHandle?
    private var response: HTTPURLResponse?
    private var body = Data()
    private var received: Int64 = 0
    private var failure: Error?
    private var continuation: CheckedContinuation<(HTTPURLResponse, Data), Error>?
    private var outcome: Result<(HTTPURLResponse, Data), Error>?
    private var lastReport = Date.distantPast
    private let progress: (Int64, Int64) -> Void

    private init(progress: @escaping (Int64, Int64) -> Void) {
        queue.maxConcurrentOperationCount = 1
        self.progress = progress
    }

    /// Writes the 200 answer of `request` into `file`; `progress(received, -1)`.
    static func download(_ request: URLRequest, into file: FileHandle,
                         progress: @escaping (Int64, Int64) -> Void) async throws {
        let stream = StreamingRequest(progress: progress)
        stream.sink = file
        let (response, body) = try await stream.run { $0.dataTask(with: request) }
        if response.statusCode != 200 { _ = try InstanceAPI.decode(body, response) }
    }

    /// Uploads `file` as the body of `request`; the JSON answer.
    static func upload(_ request: URLRequest, from file: URL,
                       progress: @escaping (Int64, Int64) -> Void) async throws -> Any {
        let stream = StreamingRequest(progress: progress)
        let (response, body) = try await stream.run { $0.uploadTask(with: request, fromFile: file) }
        return try InstanceAPI.decode(body, response)
    }

    private func run(_ make: (URLSession) -> URLSessionTask) async throws -> (HTTPURLResponse, Data) {
        try Task.checkCancellation()
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 300
        config.timeoutIntervalForResource = 7 * 24 * 3600
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.connectionProxyDictionary = [:]
        let session = URLSession(configuration: config, delegate: self, delegateQueue: queue)
        let task = make(session)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                queue.addOperation {
                    if let outcome = self.outcome {
                        continuation.resume(with: outcome)
                    } else {
                        self.continuation = continuation
                        task.resume()
                    }
                }
            }
        } onCancel: {
            task.cancel()
        }
    }

    private func finish(_ result: Result<(HTTPURLResponse, Data), Error>) {
        if let continuation {
            self.continuation = nil
            continuation.resume(with: result)
        } else {
            outcome = result
        }
    }

    private func report(_ done: Int64, _ total: Int64, force: Bool = false) {
        let now = Date()
        guard force || now.timeIntervalSince(lastReport) > 0.1 else { return }
        lastReport = now
        progress(done, total)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        self.response = response as? HTTPURLResponse
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        if let sink, response?.statusCode == 200 {
            do {
                try sink.write(contentsOf: data)
            } catch {
                failure = error
                dataTask.cancel()
                return
            }
            received += Int64(data.count)
            report(received, -1)
        } else if body.count < 4 << 20 {
            body.append(data)
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didSendBodyData bytesSent: Int64,
                    totalBytesSent: Int64, totalBytesExpectedToSend: Int64) {
        report(totalBytesSent, totalBytesExpectedToSend, force: totalBytesSent == totalBytesExpectedToSend)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        session.finishTasksAndInvalidate()
        if let error = failure ?? error {
            finish(.failure(error))
        } else if let response {
            if sink != nil { report(received, -1, force: true) }
            finish(.success((response, body)))
        } else {
            finish(.failure(URLError(.badServerResponse)))
        }
    }
}

/// The export spool: `transfer/` in the app's cache folder (0700), one 0600
/// file per transfer, deleted afterwards.
enum TransferSpool {
    static let directory: URL = Paths.isolated?.appendingPathComponent("transfer", isDirectory: true)
        ?? FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("dsh-codex/transfer", isDirectory: true)

    static func create() throws -> (URL, FileHandle) {
        Paths.ensurePrivateDirectory(directory)
        let url = directory.appendingPathComponent("\(UUID().uuidString).tar.zst")
        guard FileManager.default.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600])
        else { throw TransferError.failed("Die Zwischendatei in \(directory.path) ließ sich nicht anlegen.") }
        return (url, try FileHandle(forWritingTo: url))
    }

    /// Leftovers of a transfer the app did not finish (crash, force quit).
    static func sweep() {
        let files = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        for file in files where file.pathExtension == "zst" { try? FileManager.default.removeItem(at: file) }
    }
}

enum TransferMode: Int {
    case copy = 0, move, sync
}

/// The sheet "Ordner übertragen". One at a time: the main window keeps a
/// single instance, and a running transfer disables every input.
@MainActor
final class TransferSheet: NSObject, NSComboBoxDelegate, NSTextFieldDelegate {
    private let manager: InstanceManager
    private let panel: NSPanel
    /// "Zum Ziel wechseln".
    var onSwitch: ((String) -> Void)?

    private let sourceInstance = NSPopUpButton()
    private let sourceFolder = NSComboBox()
    private let choose = NSButton(title: "Wählen …", target: nil, action: nil)
    private let targetInstance = NSPopUpButton()
    private let targetParent = NSTextField()
    private let targetName = NSTextField()
    private let mode = NSSegmentedControl(
        labels: ["Kopieren", "Verschieben", "Synchronisieren"], trackingMode: .selectOne, target: nil, action: nil)
    private let modeHint = NSTextField(wrappingLabelWithString: "")
    private let deleteExtra = NSButton(checkboxWithTitle: "Zusätzliche Dateien am Ziel löschen", target: nil, action: nil)
    private let excludes = NSTextField()
    private let status = NSTextField(wrappingLabelWithString: "")
    private let bar = NSProgressIndicator()
    private let closeButton = NSButton(title: "Schließen", target: nil, action: nil)
    private let previewButton = NSButton(title: "Vorschau", target: nil, action: nil)
    private let startButton = NSButton(title: "Starten", target: nil, action: nil)
    private let switchButton = NSButton(title: "Zum Ziel wechseln", target: nil, action: nil)

    private var job: Task<Void, Never>?
    private var step = ""
    private var parentEdited = false
    private var nameEdited = false
    private var sourceLoad = 0
    private var targetLoad = 0
    private var finishedTarget: String?

    init(manager: InstanceManager) {
        self.manager = manager
        panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 620, height: 400),
                        styleMask: [.titled], backing: .buffered, defer: true)
        super.init()
        build()
    }

    var isRunning: Bool { job != nil }

    /// Shows the sheet on `window` with `path` of instance `sourceID` as the
    /// source; false when another sheet is open there.
    func present(on window: NSWindow, sourceID: String, path: String?) -> Bool {
        if panel.sheetParent != nil {
            panel.makeKeyAndOrderFront(nil)
            if job == nil, let path, !path.isEmpty { select(source: sourceID, path: path) }
            return true
        }
        guard window.attachedSheet == nil else { return false }
        if job == nil { TransferSpool.sweep() }
        reloadInstances()
        clearOutcome()
        select(source: sourceID, path: path)
        window.beginSheet(panel)
        return true
    }

    // MARK: - Layout

    private func label(_ text: String) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.alignment = .right
        return label
    }

    private func hint(_ field: NSTextField) -> NSTextField {
        field.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        field.textColor = .secondaryLabelColor
        return field
    }

    private func section(_ text: String) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        return label
    }

    private func grid(_ rows: [[NSView]]) -> NSGridView {
        let grid = NSGridView(views: rows)
        grid.rowSpacing = 8
        grid.columnSpacing = 10
        grid.column(at: 0).xPlacement = .trailing
        grid.column(at: 0).width = 130
        grid.column(at: 1).width = 430
        grid.rowAlignment = .firstBaseline
        return grid
    }

    private func build() {
        panel.title = "Ordner übertragen"
        for control in [sourceInstance, targetInstance] {
            control.target = self
        }
        sourceInstance.action = #selector(sourceChanged)
        targetInstance.action = #selector(targetChanged)
        sourceFolder.delegate = self
        sourceFolder.completes = true
        sourceFolder.numberOfVisibleItems = 12
        sourceFolder.placeholderString = "/pfad/zum/projekt"
        choose.bezelStyle = .rounded
        choose.target = self
        choose.action = #selector(chooseClicked)
        let folderRow = NSStackView(views: [sourceFolder, choose])
        folderRow.spacing = 8
        sourceFolder.setContentHuggingPriority(.defaultLow, for: .horizontal)
        targetParent.delegate = self
        targetName.delegate = self
        targetParent.placeholderString = "Übergeordneter Ordner am Ziel"
        targetName.placeholderString = "Ordnername"
        mode.selectedSegment = TransferMode.copy.rawValue
        mode.target = self
        mode.action = #selector(modeChanged)
        deleteExtra.target = self
        deleteExtra.action = #selector(modeChanged)
        excludes.stringValue = defaultExcludes.joined(separator: ", ")
        excludes.placeholderString = "Namen, durch Komma getrennt"

        let form = grid([
            [section("Quelle"), NSGridCell.emptyContentView],
            [label("Instanz:"), sourceInstance],
            [label("Ordner:"), folderRow],
            [section("Ziel"), NSGridCell.emptyContentView],
            [label("Instanz:"), targetInstance],
            [label("Übergeordnet:"), targetParent],
            [label("Name:"), targetName],
            [section("Vorgang"), NSGridCell.emptyContentView],
            [label("Art:"), mode],
            [NSGridCell.emptyContentView, hint(modeHint)],
            [NSGridCell.emptyContentView, deleteExtra],
            [label("Ausschließen:"), excludes],
            [NSGridCell.emptyContentView, hint(NSTextField(wrappingLabelWithString:
                "Diese Namen werden überall im Ordner übersprungen. .git und versteckte Dateien werden mit übertragen."))],
        ])
        for row in [0, 3, 7] { form.row(at: row).topPadding = row == 0 ? 0 : 6 }
        for row in [0, 3, 7] { form.cell(atColumnIndex: 0, rowIndex: row).xPlacement = .leading }

        bar.style = .bar
        bar.minValue = 0
        bar.maxValue = 1
        bar.isHidden = true
        bar.widthAnchor.constraint(equalToConstant: 580).isActive = true
        status.preferredMaxLayoutWidth = 580
        status.isSelectable = true

        closeButton.target = self
        closeButton.action = #selector(closeClicked)
        closeButton.keyEquivalent = "\u{1b}"
        previewButton.target = self
        previewButton.action = #selector(previewClicked)
        startButton.target = self
        startButton.action = #selector(startClicked)
        startButton.keyEquivalent = "\r"
        switchButton.target = self
        switchButton.action = #selector(switchClicked)
        switchButton.isHidden = true
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let buttons = NSStackView(views: [switchButton, spacer, closeButton, previewButton, startButton])
        buttons.spacing = 10

        let heading = NSTextField(labelWithString: "Ordner auf ein anderes Gerät übertragen")
        heading.font = .systemFont(ofSize: 15, weight: .semibold)
        let stack = NSStackView(views: [heading, form, bar, status, buttons])
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
        modeChanged()
    }

    private func fit() {
        panel.layoutIfNeeded()
        panel.setContentSize(panel.contentView?.fittingSize ?? panel.frame.size)
    }

    // MARK: - Choices

    private func reloadInstances() {
        for popup in [sourceInstance, targetInstance] {
            let selected = selectedID(popup)
            popup.removeAllItems()
            for instance in manager.instances {
                popup.addItem(withTitle: instance.name)
                popup.lastItem?.representedObject = instance.id
            }
            if let selected { select(popup, selected) }
        }
    }

    private func selectedID(_ popup: NSPopUpButton) -> String? { popup.selectedItem?.representedObject as? String }

    private func select(_ popup: NSPopUpButton, _ id: String) {
        if let index = popup.itemArray.firstIndex(where: { $0.representedObject as? String == id }) {
            popup.selectItem(at: index)
        }
    }

    private func name(of id: String?) -> String { id.flatMap { manager.instance($0)?.name } ?? "?" }

    private func select(source id: String, path: String?) {
        select(sourceInstance, id)
        sourceFolder.stringValue = path ?? ""
        nameEdited = false
        sourceChanged()
    }

    @objc private func sourceChanged() {
        guard let id = selectedID(sourceInstance) else { return }
        choose.isEnabled = manager.instance(id)?.kind == .local
        if selectedID(targetInstance) == nil || selectedID(targetInstance) == id,
           let other = manager.instances.first(where: { $0.id != id }) {
            select(targetInstance, other.id)
            targetChanged()
        } else if targetParent.stringValue.isEmpty {
            targetChanged()
        }
        loadFolders(of: id)
        updateName()
    }

    @objc private func targetChanged() {
        parentEdited = false
        guard let id = selectedID(targetInstance) else { return }
        targetLoad += 1
        let load = targetLoad
        Task {
            do {
                let api = try await connect(id)
                let roots = try await api.call("GET", "/api/transfer/roots") as? [String: Any]
                guard load == targetLoad, !parentEdited else { return }
                targetParent.stringValue = roots?["defaultParent"] as? String ?? roots?["home"] as? String ?? ""
            } catch {
                guard load == targetLoad, job == nil else { return }
                fail("Ziel „\(name(of: id))“: \(error.localizedDescription)")
            }
        }
    }

    private func loadFolders(of id: String) {
        sourceLoad += 1
        let load = sourceLoad
        Task {
            do {
                let api = try await connect(id)
                let rows = try await api.call("GET", "/api/workspaces") as? [[String: Any]] ?? []
                guard load == sourceLoad else { return }
                sourceFolder.removeAllItems()
                sourceFolder.addItems(withObjectValues: rows.compactMap { $0["path"] as? String })
            } catch {
                guard load == sourceLoad, job == nil else { return }
                fail("Quelle „\(name(of: id))“: \(error.localizedDescription)")
            }
        }
    }

    private func updateName() {
        guard !nameEdited else { return }
        let path = sourceFolder.stringValue.trimmingCharacters(in: .whitespaces)
        let trimmed = path.hasSuffix("/") ? String(path.dropLast()) : path
        targetName.stringValue = (trimmed as NSString).lastPathComponent
    }

    @objc private func modeChanged() {
        let selected = TransferMode(rawValue: mode.selectedSegment) ?? .copy
        deleteExtra.isEnabled = selected == .sync && job == nil
        switch selected {
        case .copy:
            modeHint.stringValue = "Legt den Ordner am Ziel neu an. Ein vorhandener, nicht leerer Zielordner wird nicht angetastet."
        case .move:
            modeHint.stringValue = "Kopiert, prüft jede Datei am Ziel und löscht erst dann den Ordner an der Quelle."
        case .sync:
            modeHint.stringValue = "Überträgt fehlende und geänderte Dateien (Größe oder Änderungszeit) von der Quelle ins Ziel."
                + (deleteExtra.state == .on ? " Dateien, die es nur am Ziel gibt, werden gelöscht." : " Dateien, die es nur am Ziel gibt, bleiben.")
        }
    }

    func comboBoxSelectionDidChange(_ notification: Notification) {
        if let value = sourceFolder.objectValueOfSelectedItem as? String { sourceFolder.stringValue = value }
        updateName()
    }

    func controlTextDidChange(_ notification: Notification) {
        let field = notification.object as AnyObject?
        if field === targetParent { parentEdited = true }
        if field === targetName { nameEdited = !targetName.stringValue.isEmpty }
        if field === sourceFolder { updateName() }
    }

    @objc private func chooseClicked() {
        let open = NSOpenPanel()
        open.canChooseDirectories = true
        open.canChooseFiles = false
        open.allowsMultipleSelection = false
        open.showsHiddenFiles = true
        let current = sourceFolder.stringValue
        if !current.isEmpty { open.directoryURL = URL(fileURLWithPath: (current as NSString).expandingTildeInPath) }
        open.beginSheetModal(for: panel) { [weak self] response in
            guard let self, response == .OK, let url = open.url else { return }
            self.sourceFolder.stringValue = url.path
            self.updateName()
        }
    }

    private var excludeList: [String] {
        excludes.stringValue.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    // MARK: - Plan

    private struct Plan {
        let sourceID: String
        let targetID: String
        let source: InstanceAPI
        let target: InstanceAPI
        let targetPath: String
        let mode: TransferMode
        let deleteExtra: Bool
        let excludes: [String]
        let manifest: Manifest
        /// The target folder's manifest; nil = it does not exist yet.
        let existing: Manifest?
        /// Entries to send; nil = all.
        let files: [String]?
        /// Target entries to delete (sync), whole folders as `name/`.
        let extras: [String]
        let added: Int
        let changed: Int

        var sendCount: Int { files?.count ?? manifest.entries.count }
        var sendBytes: Int64 {
            guard let files else { return manifest.totalBytes }
            return files.reduce(0) { $0 + (manifest.byPath[$1]?.size ?? 0) }
        }
    }

    /// The instance's API once its connection is ready (connects on demand).
    private func connect(_ id: String) async throws -> InstanceAPI {
        let deadline = Date().addingTimeInterval(90)
        while true {
            guard let connection = manager.connection(id) else {
                throw TransferError.failed("Die Instanz „\(name(of: id))“ gibt es nicht mehr.")
            }
            if let api = InstanceAPI(connection) { return api }
            if Date() > deadline {
                let why = connection.message.isEmpty ? "" : ": \(connection.message)"
                throw TransferError.failed("„\(name(of: id))“ ist nicht verbunden\(why)")
            }
            try await Task.sleep(nanoseconds: 300_000_000)
        }
    }

    private func manifest(_ api: InstanceAPI, _ path: String, _ excludes: [String]) async throws -> Manifest {
        try Manifest(await api.call("POST", "/api/transfer/manifest", json: ["path": path, "excludes": excludes]))
    }

    private func makePlan() async throws -> Plan {
        guard let sourceID = selectedID(sourceInstance), let targetID = selectedID(targetInstance) else {
            throw TransferError.failed("Bitte Quelle und Ziel wählen.")
        }
        let trim = { (s: String) in s.trimmingCharacters(in: .whitespacesAndNewlines) }
        let folder = trim(sourceFolder.stringValue)
        let parent = trim(targetParent.stringValue)
        let name = trim(targetName.stringValue)
        let absolute = { (p: String) in p.hasPrefix("/") || p == "~" || p.hasPrefix("~/") }
        guard absolute(folder) else { throw TransferError.failed("Bitte den Quellordner als absoluten Pfad angeben.") }
        guard absolute(parent) else { throw TransferError.failed("Bitte den übergeordneten Zielordner als absoluten Pfad angeben.") }
        guard !name.isEmpty, !name.contains("/"), name != ".", name != ".." else {
            throw TransferError.failed("Bitte einen gültigen Ordnernamen für das Ziel angeben.")
        }
        let targetPath = (parent as NSString).appendingPathComponent(name)
        let selectedMode = TransferMode(rawValue: mode.selectedSegment) ?? .copy
        let excludes = excludeList

        progress("Verbinde mit „\(self.name(of: sourceID))“ und „\(self.name(of: targetID))“ …")
        let source = try await connect(sourceID)
        let target = try await connect(targetID)
        step = "Dateiliste der Quelle"
        progress("Lese die Quelle …")
        let manifest = try await self.manifest(source, folder, excludes)
        if sourceID == targetID, targetPath == manifest.root || targetPath.hasPrefix(manifest.root + "/")
            || manifest.root.hasPrefix(targetPath + "/") {
            throw TransferError.failed("Quelle und Ziel überschneiden sich.")
        }
        step = "Dateiliste des Ziels"
        progress("Lese das Ziel …")
        var existing: Manifest?
        do {
            existing = try await self.manifest(target, targetPath, excludes)
        } catch TransferError.server(let status, _, _) where status == 404 {
            existing = nil
        }

        var files: [String]?
        var extras: [String] = []
        var added = 0
        var changed = 0
        switch selectedMode {
        case .copy, .move:
            if let existing, !existing.entries.isEmpty {
                throw TransferError.failed("Am Ziel gibt es „\(existing.root)“ schon mit Inhalt. Wähle einen anderen Namen oder „Synchronisieren“.")
            }
        case .sync:
            var send: [String] = []
            for entry in manifest.entries {
                guard let there = existing?.byPath[entry.path] else {
                    added += 1
                    send.append(entry.path)
                    continue
                }
                if !Manifest.same(entry, there) {
                    changed += 1
                    send.append(entry.path)
                }
            }
            files = send
            if let existing { extras = Self.extras(in: existing, missingFrom: manifest) }
        }
        return Plan(sourceID: sourceID, targetID: targetID, source: source, target: target, targetPath: targetPath,
                    mode: selectedMode, deleteExtra: deleteExtra.state == .on, excludes: excludes,
                    manifest: manifest, existing: existing, files: files, extras: extras, added: added, changed: changed)
    }

    /// Target entries the source lacks; a folder the source lacks entirely
    /// goes as one `name/` (with whatever excluded content it holds).
    private static func extras(in target: Manifest, missingFrom source: Manifest) -> [String] {
        let folders = source.folders
        var result = Set<String>()
        for entry in target.entries where source.byPath[entry.path] == nil {
            var chosen = entry.path
            var prefix = ""
            for part in entry.path.split(separator: "/").dropLast() {
                prefix = prefix.isEmpty ? String(part) : prefix + "/" + part
                if !folders.contains(prefix) {
                    chosen = prefix + "/"
                    break
                }
            }
            if chosen == entry.path, entry.kind == "dir" {
                // An empty folder the source fills is no extra.
                if folders.contains(entry.path) { continue }
                chosen = entry.path + "/"
            }
            result.insert(chosen)
        }
        return result.sorted()
    }

    private static let bytes: ByteCountFormatter = {
        let f = ByteCountFormatter()
        f.countStyle = .file
        return f
    }()

    private func size(_ n: Int64) -> String { Self.bytes.string(fromByteCount: n) }

    private func summary(_ plan: Plan) -> String {
        let source = name(of: plan.sourceID), target = name(of: plan.targetID)
        var lines = ["Quelle „\(plan.manifest.root)“ auf \(source): \(plan.manifest.entries.count) Einträge, "
                     + "\(size(plan.manifest.totalBytes))"
                     + (plan.manifest.excluded > 0 ? " (\(plan.manifest.excluded) ausgeschlossen)." : ".")]
        switch plan.mode {
        case .copy, .move:
            lines.append("Ziel „\(plan.targetPath)“ auf \(target) " + (plan.existing == nil ? "wird angelegt." : "ist leer."))
            if plan.mode == .move { lines.append("Die Quelle wird erst nach erfolgreicher Prüfung gelöscht.") }
        case .sync:
            let existing = plan.existing.map { "\($0.entries.count) Einträge" } ?? "noch nicht vorhanden"
            lines.append("Ziel „\(plan.targetPath)“ auf \(target): \(existing).")
            lines.append("\(plan.added) neu, \(plan.changed) geändert (\(size(plan.sendBytes))), "
                         + "\(plan.extras.count) nur am Ziel" + (plan.extras.isEmpty ? "." : plan.deleteExtra ? " (werden gelöscht)." : " (bleiben)."))
        }
        return lines.joined(separator: "\n")
    }

    // MARK: - Running

    private func setBusy(_ busy: Bool) {
        for control in [sourceInstance, sourceFolder, targetInstance, targetParent, targetName, mode, excludes,
                        previewButton, startButton] as [NSControl] {
            control.isEnabled = !busy
        }
        choose.isEnabled = !busy && manager.instance(selectedID(sourceInstance) ?? "")?.kind == .local
        deleteExtra.isEnabled = !busy && mode.selectedSegment == TransferMode.sync.rawValue
        closeButton.title = busy ? "Abbrechen" : "Schließen"
        if !busy { bar.stopAnimation(nil) }
    }

    private func progress(_ text: String, fraction: Double? = nil) {
        status.textColor = .labelColor
        status.stringValue = text
        bar.isHidden = false
        if let fraction {
            bar.isIndeterminate = false
            bar.doubleValue = fraction
        } else {
            bar.isIndeterminate = true
            bar.startAnimation(nil)
        }
        fit()
    }

    private func fail(_ text: String) {
        status.textColor = .systemRed
        status.stringValue = text
        bar.isHidden = true
        fit()
    }

    private func clearOutcome() {
        finishedTarget = nil
        switchButton.isHidden = true
        startButton.title = "Starten"
        status.stringValue = ""
        bar.isHidden = true
        fit()
    }

    private func launch(_ work: @escaping () async -> Void) {
        guard job == nil else { return }
        setBusy(true)
        job = Task { [weak self] in
            await work()
            self?.job = nil
            self?.setBusy(false)
        }
    }

    @objc private func previewClicked() {
        clearOutcome()
        launch { [weak self] in
            guard let self else { return }
            do {
                let plan = try await self.makePlan()
                self.status.textColor = .labelColor
                self.status.stringValue = self.summary(plan)
                self.bar.isHidden = true
                self.fit()
            } catch {
                self.report(error)
            }
        }
    }

    @objc private func startClicked() {
        guard job == nil else { return }
        guard mode.selectedSegment == TransferMode.move.rawValue else { return run() }
        let alert = NSAlert()
        alert.messageText = "Ordner verschieben?"
        alert.informativeText = "„\(sourceFolder.stringValue)“ wird auf „\(name(of: selectedID(targetInstance)))“ kopiert "
            + "und dort geprüft. Danach wird der Ordner auf „\(name(of: selectedID(sourceInstance)))“ gelöscht."
        alert.addButton(withTitle: "Verschieben")
        alert.addButton(withTitle: "Abbrechen")
        alert.beginSheetModal(for: panel) { [weak self] response in
            if response == .alertFirstButtonReturn { self?.run() }
        }
    }

    private func run() {
        clearOutcome()
        launch { [weak self] in await self?.perform() }
    }

    private func report(_ error: Error) {
        if error is CancellationError || (error as? URLError)?.code == .cancelled {
            fail("Abgebrochen. Am Ziel kann ein unvollständiger Ordner zurückbleiben.")
        } else {
            fail("\(step) fehlgeschlagen: \(error.localizedDescription)")
        }
        startButton.title = "Erneut versuchen"
    }

    private func perform() async {
        let started = Date()
        var spool: URL?
        defer {
            if let spool { try? FileManager.default.removeItem(at: spool) }
        }
        do {
            step = "Vorbereitung"
            let plan = try await makePlan()
            let sourceName = name(of: plan.sourceID), targetName = name(of: plan.targetID)
            appLog.note("transfer: \(plan.mode) \(plan.manifest.root) (\(plan.sourceID)) → \(plan.targetPath) (\(plan.targetID)), \(plan.sendCount) entries")
            var written: Int64 = 0
            var sent: Int64 = 0
            var targetRoot = plan.existing?.root ?? plan.targetPath
            if plan.sendCount > 0 {
                step = "Export"
                progress("Export von \(sourceName) …")
                let (file, handle) = try TransferSpool.create()
                spool = file
                var body: [String: Any] = ["path": plan.manifest.root, "excludes": plan.excludes]
                if let files = plan.files { body["files"] = files }
                do {
                    try await StreamingRequest.download(
                        plan.source.request("POST", "/api/transfer/export", json: body), into: handle
                    ) { received, _ in
                        DispatchQueue.main.async {
                            MainActor.assumeIsolated {
                                self.progress("Export von \(sourceName): \(self.size(received)) komprimiert geladen …")
                            }
                        }
                    }
                    try handle.close()
                } catch {
                    try? handle.close()
                    throw error
                }

                step = "Import"
                var request = plan.target.request(
                    "POST", "/api/transfer/import",
                    query: [("path", plan.targetPath), ("mode", plan.mode == .sync ? "merge" : "fresh")])
                request.setValue("application/x-tar+zstd", forHTTPHeaderField: "Content-Type")
                let result = try await StreamingRequest.upload(request, from: file) { done, total in
                    DispatchQueue.main.async {
                        MainActor.assumeIsolated {
                            let fraction = total > 0 ? Double(done) / Double(total) : nil
                            self.progress("Übertrage nach \(targetName): \(self.size(done))"
                                          + (total > 0 ? " von \(self.size(total))" : "") + " …", fraction: fraction)
                        }
                    }
                } as? [String: Any]
                written = (result?["written"] as? NSNumber)?.int64Value ?? 0
                sent = (result?["bytes"] as? NSNumber)?.int64Value ?? 0
                if let path = result?["path"] as? String { targetRoot = path }
                try? FileManager.default.removeItem(at: file)
                spool = nil

                step = "Prüfung"
                progress("Prüfe \(plan.sendCount) Einträge am Ziel …")
                let after = try await manifest(plan.target, targetRoot, plan.excludes)
                let wanted = plan.files.map(Set.init)
                let bad = plan.manifest.entries.filter { entry in
                    guard wanted?.contains(entry.path) ?? true else { return false }
                    guard let there = after.byPath[entry.path] else { return true }
                    return !Manifest.same(entry, there)
                }.map(\.path)
                guard bad.isEmpty else {
                    throw TransferError.failed("\(bad.count) Einträge stimmen am Ziel nicht überein (z. B. "
                        + bad.prefix(3).joined(separator: ", ") + "). Die Quelle bleibt unverändert.")
                }
            }

            var deleted = 0
            if plan.mode == .sync, plan.deleteExtra, !plan.extras.isEmpty {
                step = "Löschen am Ziel"
                progress("Lösche \(plan.extras.count) zusätzliche Einträge am Ziel …")
                let answer = try await plan.target.call(
                    "POST", "/api/transfer/delete", json: ["path": targetRoot, "files": plan.extras]) as? [String: Any]
                deleted = (answer?["deleted"] as? NSNumber)?.intValue ?? 0
            }

            step = "Eintragen am Ziel"
            _ = try await plan.target.call("POST", "/api/workspaces", json: ["path": targetRoot])

            if plan.mode == .move {
                step = "Löschen der Quelle"
                progress("Lösche die Quelle auf \(sourceName) …")
                _ = try await plan.source.call("POST", "/api/transfer/remove", json: ["path": plan.manifest.root])
            }

            let seconds = max(1, Int(Date().timeIntervalSince(started).rounded()))
            var lines = [plan.sendCount == 0
                ? "Das Ziel ist bereits aktuell."
                : "\(written) Einträge (\(size(sent))) in \(seconds) s nach „\(targetRoot)“ auf \(targetName) übertragen und geprüft."]
            if deleted > 0 { lines.append("\(deleted) zusätzliche Einträge am Ziel gelöscht.") }
            if plan.mode == .move { lines.append("Der Ordner auf \(sourceName) wurde gelöscht.") }
            appLog.note("transfer: done, \(written) entries, \(sent) bytes, \(seconds) s")
            status.textColor = .labelColor
            status.stringValue = lines.joined(separator: "\n")
            bar.isIndeterminate = false
            bar.doubleValue = 1
            finishedTarget = plan.targetID
            switchButton.isHidden = plan.targetID == manager.currentID
            fit()
        } catch {
            appLog.note("transfer: \(step) failed: \(error.localizedDescription)")
            report(error)
        }
    }

    @objc private func closeClicked() {
        if let job {
            job.cancel()
            status.stringValue = "Wird abgebrochen …"
            return
        }
        close()
    }

    @objc private func switchClicked() {
        guard let id = finishedTarget else { return }
        close()
        onSwitch?(id)
    }

    private func close() {
        if let parent = panel.sheetParent { parent.endSheet(panel) } else { panel.orderOut(nil) }
    }
}
