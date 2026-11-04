import SwiftUI

/// Codex-like transcript rows for the dsh tools (ui/src/components/tools.js): one quiet line each
/// (icon, verb, dot, detail); a tap opens the body.
enum ToolRows {
    static func resultText(_ r: ToolResult?) -> String { r?.text ?? "" }

    static func errorReason(_ r: ToolResult?) -> String {
        if let reason = r?.error?["reason"]?.string, !reason.isEmpty { return reason }
        let text = resultText(r).replacingOccurrences(of: #"^Error:\s*"#, with: "", options: .regularExpression)
        return text.isEmpty ? L.toolFailed : text
    }

    static func ext(_ path: String) -> String {
        guard let r = path.range(of: #"\.([a-z0-9+]+)$"#, options: [.regularExpression, .caseInsensitive]) else { return "" }
        return String(path[r].dropFirst()).lowercased()
    }

    /// Diff hunks of an edit or write: the tool's own diffs, else the arguments.
    static func editHunks(_ item: ToolItem) -> [[Diff.Row]] {
        if let diffs = item.result?.meta?["diffs"]?.array, !diffs.isEmpty {
            return diffs.map { Diff.lines($0["oldText"]?.string, $0["newText"]?.string) }
        }
        if item.name == "write" { return [Diff.lines(nil, item.args["content"]?.string ?? "")] }
        if item.name == "edit", item.args["old_string"]?.string != nil || item.args["new_string"]?.string != nil {
            let old = item.args["old_string"]?.string
            return [Diff.lines(old?.isEmpty == false ? old : nil, item.args["new_string"]?.string ?? "")]
        }
        return []
    }

    /// Absolute or relative path an edit/write touched.
    static func editPath(_ item: ToolItem) -> String {
        item.result?.meta?["path"]?.string ?? item.args["file_path"]?.string ?? item.result?.meta?["diffs"]?[0]?["path"]?.string ?? ""
    }

    static let automation = try! NSRegularExpression(pattern: #"^(schedule_|team_task_)|^(create_goal|update_goal|get_goal|spawn_teammate|list_agents|wait_agent|interrupt_agent)$"#)
}

/// Picks the row for a tool call.
struct ToolRowView: View {
    let item: ToolItem
    let cwd: String
    let client: any ServerAPI

    var body: some View {
        let n = item.name
        if ToolRows.automation.firstMatch(in: n, range: NSRange(location: 0, length: (n as NSString).length)) != nil {
            AutomationRow(item: item)
        } else if Reduce.codexTools.contains(n) {
            CodexRow(item: item, client: client)
        } else if n == "bash" {
            BashRow(item: item)
        } else if n == "read" {
            ReadRow(item: item, cwd: cwd)
        } else if n == "edit" || n == "write" {
            EditRow(item: item, cwd: cwd)
        } else if n == "glob" || n == "grep" {
            SearchRow(item: item, cwd: cwd)
        } else if n == "todo_write" {
            TodoRow(item: item)
        } else if n == "web_fetch" || n == "web_search" {
            WebRow(item: item)
        } else if n == "read_image" {
            ImageRow(item: item, cwd: cwd, client: client)
        } else {
            GenericRow(item: item)
        }
    }
}

/// The one-line header of a row and its expandable body.
struct ToolRow<Content: View>: View {
    var icon: String
    var title: String
    var summary: String = ""
    var mono = false
    var running = false
    var error = false
    var badge: String? = nil
    var badgeTone: Color = Theme.textMuted
    var expandable = false
    @Binding var open: Bool
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                guard expandable else { return }
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: icon).font(.footnote).foregroundStyle(error ? Theme.danger : Theme.textMuted).frame(width: 16)
                    ShimmerText(text: title, active: running)
                        .layoutPriority(2)
                    if !summary.isEmpty {
                        Text("·").foregroundStyle(Theme.textMuted)
                        Text(summary)
                            .font(mono ? .footnote.monospaced() : .footnote)
                            .foregroundStyle(error ? Theme.danger : Theme.textMuted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    if let badge {
                        Text(badge).font(.caption2.weight(.medium)).foregroundStyle(badgeTone)
                            .padding(.horizontal, 6).padding(.vertical, 2)
                            .background(badgeTone.opacity(0.12), in: Capsule())
                            .layoutPriority(1)
                    }
                    Spacer(minLength: 0)
                    if expandable {
                        Image(systemName: "chevron.right").font(.caption2.weight(.semibold)).foregroundStyle(Theme.textMuted)
                            .rotationEffect(.degrees(open ? 90 : 0))
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!expandable)
            .accessibilityElement(children: .combine)
            .accessibilityLabel([title, summary, badge ?? ""].filter { !$0.isEmpty }.joined(separator: ", "))
            .accessibilityValue(expandable ? (open ? L.chatA11yCollapse : L.chatA11yExpand) : "")
            if open, expandable {
                content().padding(.leading, 24)
            }
        }
    }
}

/// Long outputs show their first lines; the rest is one tap away.
struct Clamp<Content: View>: View {
    let lines: [String]
    var max = 14
    @ViewBuilder var render: ([String]) -> Content
    @State private var all = false

    var body: some View {
        let shown = all || lines.count <= max + 3 ? lines : Array(lines.prefix(max))
        VStack(alignment: .leading, spacing: 4) {
            render(shown)
            if lines.count > max + 3 {
                Button(all ? L.commonShowLess : L.commonShowMore(lines.count - max)) { withAnimation(.snappy) { all.toggle() } }
                    .font(.caption).foregroundStyle(Theme.accent)
            }
        }
    }
}

struct CodePanel: View {
    let text: String
    var tone: Color = Theme.text

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text)
                .font(.caption.monospaced())
                .foregroundStyle(tone)
                .textSelection(.enabled)
                .padding(10)
        }
        .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
    }
}

// MARK: - bash

struct BashRow: View {
    let item: ToolItem
    @State private var open = false

    var body: some View {
        let text = ToolRows.resultText(item.result)
        let out = BashOutput.splitPersistent(text) ?? BashOutput.split(text)
        let running = item.result == nil && !item.isWaiting
        let failed = item.failed
        let exit = failed ? nil : out.exit
        let waiting = item.isWaiting
        let badge: String? = waiting ? L.toolWaitingApproval : (item.approval?.isDenied ?? false) ? L.toolDenied
            : (exit != nil && exit != 0) ? L.toolExit(exit!) : (out.note != nil && !failed ? out.note : nil)
        let tone: Color = waiting ? Theme.warning : (exit != nil && exit != 0) || out.noteError ? Theme.danger : Theme.textMuted
        ToolRow(icon: "terminal", title: running || waiting ? L.toolRunning : L.toolRan, summary: item.args["command"]?.string ?? "", mono: true,
                running: running, error: failed, badge: badge, badgeTone: tone, expandable: item.result != nil, open: $open) {
            VStack(alignment: .leading, spacing: 6) {
                CodePanel(text: "$ " + (item.args["command"]?.string ?? ""), tone: Theme.textMuted)
                if failed {
                    CodePanel(text: ToolRows.errorReason(item.result), tone: Theme.danger)
                } else {
                    if !out.stdout.isEmpty {
                        Clamp(lines: out.stdout.components(separatedBy: "\n")) { CodePanel(text: $0.joined(separator: "\n")) }
                    }
                    if !out.stderr.isEmpty {
                        Clamp(lines: out.stderr.components(separatedBy: "\n")) { CodePanel(text: $0.joined(separator: "\n"), tone: Theme.warning) }
                    }
                    if out.stdout.isEmpty, out.stderr.isEmpty { Text(L.toolNoOutput).font(.caption).foregroundStyle(Theme.textMuted) }
                    let foot = [exit.map { L.toolExit($0) }, out.note].compactMap { $0 }.joined(separator: " · ")
                    if !foot.isEmpty { Text(foot).font(.caption).foregroundStyle((exit ?? 0) != 0 || out.noteError ? Theme.danger : Theme.textMuted) }
                }
            }
        }
    }
}

// MARK: - read

struct ReadRow: View {
    let item: ToolItem
    let cwd: String
    @State private var open = false

    var body: some View {
        let meta = item.result?.meta
        let running = item.result == nil
        let failed = item.failed
        let path = Format.relPath(meta?["path"]?.string ?? item.args["file_path"]?.string ?? "", cwd: cwd)
        let lines = meta?["lines"]?.array ?? []
        let range = lines.isEmpty ? nil : L.toolLines(lines.first?["number"]?.int ?? 0, lines.last?["number"]?.int ?? 0)
        ToolRow(icon: "doc.text", title: running ? L.toolReading : L.toolRead, summary: failed ? ToolRows.errorReason(item.result) : path,
                mono: !failed, running: running, error: failed, badge: failed ? nil : range, expandable: !lines.isEmpty, open: $open) {
            Clamp(lines: lines.map { "\($0["number"]?.int ?? 0)\t\($0["text"]?.string ?? "")" }, max: 18) { CodePanel(text: $0.joined(separator: "\n")) }
        }
    }
}

// MARK: - write / edit

struct EditRow: View {
    let item: ToolItem
    let cwd: String
    @State private var open = false

    var body: some View {
        let running = item.result == nil
        let failed = item.failed
        let meta = item.result?.meta
        let path = Format.relPath(ToolRows.editPath(item), cwd: cwd)
        let create = item.name == "write" && (meta?["operation"]?.string == "create" || ((meta?["diffs"]?.array?.isEmpty ?? true) && running))
        let hunks = ToolRows.editHunks(item)
        let counts = Diff.countChanges(hunks.flatMap { $0 })
        let title = create ? (running ? L.toolCreating : L.toolCreated) : running ? L.toolEditing : L.toolEdited
        let badge: String? = item.isWaiting ? L.toolWaitingApproval : (item.approval?.isDenied ?? false) ? L.toolDenied
            : (!failed && (counts.add > 0 || counts.del > 0)) ? [counts.add > 0 ? "+\(counts.add)" : nil, counts.del > 0 ? "−\(counts.del)" : nil].compactMap { $0 }.joined(separator: " ") : nil
        ToolRow(icon: "pencil", title: title, summary: failed ? "\(path) · \(ToolRows.errorReason(item.result))" : path, mono: !failed,
                running: running, error: failed, badge: badge, badgeTone: item.isWaiting ? Theme.warning : Theme.online,
                expandable: !hunks.isEmpty && !failed, open: $open) {
            DiffView(hunks: hunks)
        }
    }
}

// MARK: - glob / grep

struct SearchRow: View {
    let item: ToolItem
    let cwd: String
    @State private var open = false

    /// The badge ("3 Dateien") and the expandable lines of a search result.
    private var outcome: (count: String, lines: [String]) {
        let failed = item.failed
        let meta = item.result?.meta
        var count = ""
        var lines: [String] = []
        if meta?["shape"]?.string == "paths" {
            let total = meta?["total"]?.int ?? 0
            count = total > 0 ? L.toolFiles(total) : L.toolNoMatches
            lines = (meta?["paths"]?.array ?? []).compactMap(\.string).map { Format.relPath($0, cwd: cwd) }
        } else if meta?["shape"]?.string == "matches" {
            let total = meta?["total"]?.int ?? 0
            let files = meta?["files"]?.array ?? []
            count = total > 0 ? L.toolIn(L.toolMatchCount(total), L.toolFiles(files.count)) : L.toolNoMatches
            for f in files {
                lines.append(Format.relPath(f["path"]?.string ?? "", cwd: cwd))
                for m in (f["matches"]?.array ?? []).prefix(20) { lines.append("  \(m["lineNumber"]?.int ?? 0): \(m["line"]?.string ?? "")") }
            }
        } else if item.result != nil, !failed {
            lines = ToolRows.resultText(item.result).components(separatedBy: "\n")
        }
        return (count, lines)
    }

    var body: some View {
        let running = item.result == nil
        let failed = item.failed
        let (count, lines) = outcome
        let what = item.args["pattern"]?.string ?? ""
        let whereIn = (item.args["path"]?.string).flatMap { $0 == "." ? nil : " in \(Format.relPath($0, cwd: cwd))" } ?? ""
        let include = item.args["include"]?.string.map { " (\($0))" } ?? ""
        ToolRow(icon: "magnifyingglass", title: running ? L.toolSearching : L.toolSearched,
                summary: failed ? ToolRows.errorReason(item.result) : what + whereIn + include, mono: !failed, running: running, error: failed,
                badge: count.isEmpty || failed ? nil : count, expandable: !lines.isEmpty, open: $open) {
            Clamp(lines: lines) { CodePanel(text: $0.joined(separator: "\n")) }
        }
    }
}

// MARK: - todo

struct TodoRow: View {
    let item: ToolItem
    @State private var open: Bool

    init(item: ToolItem) {
        self.item = item
        _open = State(initialValue: item.latestTodo)
    }

    private var todos: [Todo] {
        item.todos ?? (item.args["todos"]?.array ?? []).compactMap(Todo.init(json:))
    }

    var body: some View {
        let list = todos
        let done = list.filter { $0.status == "completed" }.count
        ToolRow(icon: "list.bullet", title: L.chatUpdatedPlan, summary: L.chatPlanDone(done, list.count), running: item.result == nil,
                expandable: !list.isEmpty, open: $open) {
            TodoList(todos: list)
        }
    }
}

struct TodoList: View {
    let todos: [Todo]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(todos.enumerated()), id: \.offset) { _, t in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: t.status == "completed" ? "checkmark.circle.fill" : t.status == "in_progress" ? "circle.inset.filled" : "circle")
                        .font(.footnote)
                        .foregroundStyle(t.status == "pending" ? Theme.textMuted : Theme.accent)
                        .accessibilityLabel(t.status == "completed" ? L.chatTodoCompleted : t.status == "in_progress" ? L.chatTodoInProgress : L.chatTodoPending)
                    Text(t.content).font(.footnote).foregroundStyle(t.status == "completed" ? Theme.textMuted : Theme.text)
                }
            }
        }
    }
}

// MARK: - web

struct WebRow: View {
    let item: ToolItem
    @State private var open = false

    var body: some View {
        let running = item.result == nil
        let failed = item.failed
        let text = ToolRows.resultText(item.result)
        let fetch = item.name == "web_fetch"
        let what: String = fetch
            ? (item.args["url"]?.string ?? "").replacingOccurrences(of: #"^https?://"#, with: "", options: .regularExpression).replacingOccurrences(of: #"/$"#, with: "", options: .regularExpression)
            : ((item.args["queries"]?.array?.compactMap(\.string)) ?? [item.args["query"]?.string].compactMap { $0 }).joined(separator: " · ")
        let title = fetch ? (running ? L.toolFetching : L.toolFetched) : (running ? L.toolSearchingWeb : L.toolSearchedWeb)
        ToolRow(icon: "globe", title: title, summary: failed ? "\(Format.clip(what, 60)) · \(ToolRows.errorReason(item.result))" : what,
                running: running, error: failed, expandable: !text.isEmpty && !failed, open: $open) {
            Clamp(lines: text.components(separatedBy: "\n")) { CodePanel(text: $0.joined(separator: "\n")) }
        }
    }
}

struct ImageRow: View {
    let item: ToolItem
    let cwd: String
    let client: any ServerAPI
    @State private var open = true

    var body: some View {
        let image = item.result?.image
        ToolRow(icon: "photo", title: L.toolViewedImage, summary: Format.relPath(item.args["file_path"]?.string ?? "", cwd: cwd), mono: true,
                running: item.result == nil, error: item.failed, expandable: image != nil, open: $open) {
            if let image {
                AttachmentImage(ref: image, client: client)
                    .frame(maxWidth: 320, maxHeight: 240)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
            }
        }
    }
}

// MARK: - anything else

struct GenericRow: View {
    let item: ToolItem
    @State private var open = false

    var body: some View {
        let running = item.result == nil
        let failed = item.failed
        let text = ToolRows.resultText(item.result)
        let detail = item.args["description"]?.string ?? item.args["job_id"]?.string ?? item.args["name"]?.string ?? item.args["agent_id"]?.string ?? ""
        let badge: String? = item.isWaiting ? L.toolWaitingApproval : (item.approval?.isDenied ?? false) ? L.toolDenied : nil
        ToolRow(icon: item.name.hasPrefix("job_") ? "clock.arrow.circlepath" : "wrench.and.screwdriver", title: item.name,
                summary: failed ? ToolRows.errorReason(item.result) : detail, running: running, error: failed, badge: badge,
                badgeTone: item.isWaiting ? Theme.warning : Theme.textMuted, expandable: true, open: $open) {
            VStack(alignment: .leading, spacing: 6) {
                CodePanel(text: item.args.encoded(pretty: true), tone: Theme.textMuted)
                if !text.isEmpty { Clamp(lines: text.components(separatedBy: "\n")) { CodePanel(text: $0.joined(separator: "\n")) } }
            }
        }
    }
}

// MARK: - automations, goals, teams

struct AutomationRow: View {
    let item: ToolItem
    @State private var open = false

    var body: some View {
        let running = item.result == nil
        let text = ToolRows.resultText(item.result)
        let out = JSONValue.parse(text)
        // Validation problems come back as `{code, message}` results, not as errors.
        let problem: String? = item.failed ? ToolRows.errorReason(item.result) : (out?["code"] != nil && out?["message"]?.string != nil) ? out?["message"]?.string : nil
        let view = out?["goal"] ?? out
        let detail: String = {
            if item.name.hasPrefix("schedule_"), let title = view?["title"]?.string { return title }
            if item.name.hasSuffix("_goal") { return view?["objective"]?.string ?? item.args["objective"]?.string ?? "" }
            return item.args["name"]?.string ?? item.args["target"]?.string ?? item.args["subject"]?.string ?? item.args["task_id"]?.string ?? ""
        }()
        ToolRow(icon: item.name.hasPrefix("schedule_") ? "clock.arrow.circlepath" : "wrench.and.screwdriver",
                title: L.lookup("tool.label.\(item.name)") ?? item.name, summary: problem ?? detail, running: running, error: problem != nil,
                expandable: !text.isEmpty, open: $open) {
            CodePanel(text: text)
        }
    }
}

// MARK: - the Mac tools: computer, app, browser

struct CodexRow: View {
    let item: ToolItem
    let client: any ServerAPI
    @State private var enlarged = false

    private var args: JSONValue { item.args }

    private var title: String {
        let meta = item.result?.meta
        let place = meta?["place"]?.string ?? (item.name == "app" ? (args["app"]?.string ?? item.prevShot?.place) : nil) ?? ""
        return item.name == "computer" ? L.codexComputer : item.name == "browser" ? L.codexBrowser : (place.isEmpty ? L.codexApp : place)
    }

    private var action: String {
        item.name == "computer" ? CodexLabels.computer(args) : CodexLabels.background(tool: item.name, args: args, target: item.result?.meta?["target"]?.string)
    }

    var body: some View {
        let running = item.result == nil
        let failed = item.failed
        let image = item.result?.image ?? (running ? item.prevShot?.image : nil)
        let place = item.result?.meta?["place"]?.string ?? ""
        let summary = item.name == "browser" && !place.isEmpty && !["open", "new_tab"].contains(args["action"]?.string ?? "") ? "\(action) · \(place)" : action
        let error = failed ? (item.result?.error?["reason"]?.string ?? ToolRows.resultText(item.result).nilIfEmpty ?? L.toolFailed) : nil
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: item.name == "computer" ? "cursorarrow" : item.name == "browser" ? "globe" : "macwindow")
                    .font(.footnote).foregroundStyle(failed ? Theme.danger : Theme.textMuted).frame(width: 16)
                ShimmerText(text: title, active: running)
                Text("·").foregroundStyle(Theme.textMuted)
                Text(error ?? summary).font(.footnote).foregroundStyle(failed ? Theme.danger : Theme.textMuted).lineLimit(1)
                Spacer(minLength: 0)
            }
            .accessibilityElement(children: .combine)
            if let image {
                AttachmentImage(ref: image, client: client)
                    .frame(maxWidth: 360, maxHeight: 220)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous).stroke(Theme.border))
                    .opacity(running ? 0.6 : 1)
                    .onTapGesture { enlarged = true }
                    .accessibilityLabel(L.chatA11yImage)
                    .accessibilityAddTraits(.isButton)
                    .sheet(isPresented: $enlarged) {
                        NavigationStack {
                            AttachmentImage(ref: image, client: client)
                                .padding()
                                .navigationTitle(title)
                                .navigationBarTitleDisplayMode(.inline)
                                .toolbar { ToolbarItem(placement: .confirmationAction) { Button(L.commonDone) { enlarged = false } } }
                        }
                    }
            }
        }
    }
}

/// Step labels of the Mac tools (ui/src/components/codex.js).
enum CodexLabels {
    static func computer(_ args: JSONValue) -> String {
        let at = args["x"]?.isNumber == true ? " (\(Int(args["x"]?.double ?? 0)), \(Int(args["y"]?.double ?? 0)))" : ""
        let text = args["text"]?.string ?? ""
        switch args["action"]?.string ?? "" {
        case "screenshot": return L.codexScreenshot
        case "click": return L.codexClick + at
        case "double_click": return L.codexDouble + at
        case "right_click": return L.codexRight + at
        case "move": return L.codexMove + at
        case "drag": return "\(L.codexDrag)\(at) → (\(Int(args["to_x"]?.double ?? 0)), \(Int(args["to_y"]?.double ?? 0)))"
        case "scroll": return ((args["dy"]?.double ?? 0) > 0 ? L.codexScrollDown : L.codexScrollUp) + at
        case "type": return "\(L.codexType) \(Format.quote(Format.clip(text, 48)))"
        case "key": return "\(L.codexKey) \(text)"
        case "open_app": return "\(L.codexOpen) \(text)"
        case "wait": return "\(L.codexWait) \(args["seconds"]?.int ?? 1)s"
        case "cursor_position": return L.codexCursor
        default: return args["action"]?.string ?? ""
        }
    }

    static func background(tool: String, args: JSONValue, target: String?) -> String {
        let at: String = {
            if let target, !target.isEmpty { return " \(Format.quote(Format.clip(target, 48)))" }
            if let i = args["index"]?.int { return " [\(i)]" }
            if args["x"]?.isNumber == true { return " (\(Int(args["x"]?.double ?? 0)), \(Int(args["y"]?.double ?? 0)))" }
            return ""
        }()
        let text = args["text"]?.string ?? ""
        let url = (args["url"]?.string ?? "").replacingOccurrences(of: #"^https?://"#, with: "", options: .regularExpression).replacingOccurrences(of: #"/$"#, with: "", options: .regularExpression)
        switch args["action"]?.string ?? "" {
        case "open": return "\(L.codexOpen) \(Format.clip(url, 60))"
        case "state": return tool == "app" ? L.codexBWindowState : L.codexBState
        case "click": return ((args["count"]?.int ?? 1) > 1 ? L.codexDouble : L.codexClick) + at
        case "type": return "\(L.codexType) \(Format.quote(Format.clip(text, 48)))" + (target.map { " \(L.codexInto) \(Format.quote(Format.clip($0, 40)))" } ?? at)
        case "key": return "\(L.codexKey) \(args["keys"]?.string ?? "")"
        case "scroll": return L.codexBScroll + at
        case "select": return "\(L.codexBSelect) \(Format.quote(Format.clip(args["value"]?.string ?? "", 40)))" + at
        case "set_value": return L.codexBSetValue + at
        case "menu": return "\(L.codexBMenu) \((args["path"]?.array ?? []).compactMap(\.string).joined(separator: " › "))"
        case "action": return (args["name"]?.string ?? "") + at
        case "read": return L.codexBRead
        case "back": return L.codexBBack
        case "forward": return L.codexBForward
        case "reload": return L.codexBReload
        case "tabs": return L.codexBTabs
        case "new_tab": return "\(L.codexBNewTab) \(Format.clip(url, 50))"
        case "switch_tab": return "\(L.codexBTab) \(args["tab"]?.int.map(String.init) ?? args["tab"]?.string ?? "")"
        case "close_tab": return "\(L.codexBCloseTab) \(args["tab"]?.int.map(String.init) ?? args["tab"]?.string ?? "")"
        case "wait": return "\(L.codexWait) \(args["seconds"]?.int ?? 1)s"
        case "list_apps": return L.codexBApps
        case "show": return tool == "app" ? L.codexBShowApp : L.codexBShowBrowser
        case "hide": return L.codexBHideBrowser
        default: return args["action"]?.string ?? ""
        }
    }
}

extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
