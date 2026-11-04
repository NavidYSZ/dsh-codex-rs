import Foundation

// Port of ui/src/worklog.js: what kind of work a tool call is, how a turn's items fold into work groups,
// the one status line that stands for a run of steps, and what a finished turn folds into.

/// dsh `activity`, plus the Mac tools, MCP servers (`mcp:<server>`) and skills.
enum Activity {
    static let team: Set<String> = ["spawn_teammate", "send_message", "list_agents", "wait_agent", "interrupt_agent"]

    static func of(_ name: String) -> String {
        if name == "read" { return "read" }
        if name == "read_image" { return "readImage" }
        if name == "grep" || name == "glob" || name.hasSuffix("_inspect") { return "search" }
        if name == "write" { return "write" }
        if name == "edit" || name == "apply_patch" { return "edit" }
        if ["bash", "pwsh", "exec_command", "write_stdin"].contains(name) || name.hasPrefix("terminal_") || name.hasPrefix("job_") { return "commands" }
        if name == "run_code" { return "code" }
        if name == "web_search" { return "webSearch" }
        if name == "web_fetch" { return "webFetch" }
        if name == "subagent" || name.hasPrefix("subagent_") || name.hasPrefix("team_task_") || team.contains(name) { return "subagents" }
        if ["todo_write", "create_goal", "update_goal", "get_goal"].contains(name) { return "plan" }
        if ["ask_user", "ask_user_question", "request_user_input"].contains(name) { return "questions" }
        if name == "browser" { return "browser" }
        if name == "computer" || name == "app" { return "mac" }
        if name == "skill" { return "skill" }
        if name.hasPrefix("mcp__") {
            let rest = name.dropFirst(5)
            if let end = rest.range(of: "__") { return "mcp:" + rest[..<end.lowerBound] }
        }
        return "tools"
    }

    enum Table: String { case live, prepare, done }

    /// The localized phrase of an activity from one of the three tables.
    static func label(_ table: Table, _ kind: String) -> String {
        if kind.hasPrefix("mcp:") {
            let server = String(kind.dropFirst(4))
            switch table {
            case .live: return L.workLiveMcp(server)
            case .prepare: return L.workPrepareMcp(server)
            case .done: return L.workDoneMcp(server)
            }
        }
        return L.lookup("work.\(table.rawValue).\(kind)") ?? L.lookup("work.\(table.rawValue).tools") ?? kind
    }

    /// dsh `processTitle`: the top three kinds of a finished group, without counts.
    static func processTitle(_ counts: [KindCount]) -> String {
        let labels = counts.prefix(3).map { label(.done, $0.kind) }
        if labels.isEmpty { return L.workDoneThinking }
        // English continues in lower case ("Read files and ran commands"); German nouns keep theirs.
        let german = Locale.current.language.languageCode?.identifier == "de"
        let next = { (s: String) -> String in german ? s : (s.prefix(1).lowercased() + s.dropFirst()) }
        if labels.count == 1 { return labels[0] }
        if labels.count == 2 { return L.workJoin(labels[0], next(labels[1])) }
        let title = ([labels[0]] + labels.dropFirst().map(next)).joined(separator: ", ")
        return counts.count > 3 ? L.workMore(title) : title
    }
}

struct KindCount: Hashable, Sendable {
    var kind: String
    var count: Int
}

/// A run of reasoning and tool items behind one status line, or a single item.
enum Segment: Hashable, Sendable, Identifiable {
    case item(TranscriptItem)
    case group(WorkGroup)

    var id: String {
        switch self {
        case .item(let x): return x.key
        case .group(let g): return g.key
        }
    }
}

struct WorkGroup: Hashable, Sendable {
    var key: String
    var items: [TranscriptItem]
    var closed: Bool
    var counts: [KindCount]
}

/// What a finished turn folds into (dsh `turnProcess`).
struct TurnWork: Hashable, Sendable {
    var answerKey: String?
    var toolCalls: Int
    var messages: Int
    var steered: Bool
    var hidden: Bool
}

enum Worklog {
    /// Distinct tool calls per kind, most frequent first (ties by first appearance).
    static func countKinds(_ items: [TranscriptItem]) -> [KindCount] {
        var order: [String] = []
        var counts: [String: Int] = [:]
        for x in items {
            guard case .tool(let t) = x else { continue }
            let kind = Activity.of(t.name)
            if counts[kind] == nil { order.append(kind) }
            counts[kind, default: 0] += 1
        }
        return order.enumerated().map { (i, kind) in (i, KindCount(kind: kind, count: counts[kind] ?? 0)) }
            .sorted { a, b in a.1.count != b.1.count ? a.1.count > b.1.count : a.0 < b.0 }
            .map(\.1)
    }

    /// Splits a turn's items into segments: runs of reasoning and tool items become one group (`g:<n>`,
    /// numbered within the turn); everything else stands alone. A group is closed once something follows it or the turn ended.
    static func groupItems(_ items: [TranscriptItem], ended: Bool) -> (segments: [Segment], groups: Int) {
        var segments: [Segment] = []
        var pending: [TranscriptItem] = []
        var groups = 0
        func flush(_ closed: Bool) {
            guard !pending.isEmpty else { return }
            segments.append(.group(WorkGroup(key: "g:\(groups)", items: pending, closed: closed, counts: countKinds(pending))))
            groups += 1
            pending = []
        }
        for x in items {
            switch x {
            case .reasoning, .tool: pending.append(x)
            default:
                flush(true)
                segments.append(.item(x))
            }
        }
        flush(ended)
        return (segments, groups)
    }

    /// What a finished turn folds into: the final answer stays, the rest hides behind "Completed in …".
    static func turnWork(_ items: [TranscriptItem]) -> TurnWork {
        var answer = -1
        for (i, x) in items.enumerated() where x.isText { answer = i }
        var toolCalls = 0, messages = 0, users = 0
        var hidden = false
        for (i, x) in items.enumerated() {
            switch x {
            case .tool: toolCalls += 1
            case .text: if i < answer { messages += 1 }
            case .user: users += 1
            default: break
            }
            if i != answer, x.isReasoning || x.isTool || x.isText { hidden = true }
        }
        return TurnWork(answerKey: answer >= 0 ? items[answer].key : nil, toolCalls: toolCalls, messages: messages, steered: users > 1, hidden: hidden)
    }

    /// Items that stay visible in a folded turn besides its answer.
    static func staysVisible(_ item: TranscriptItem) -> Bool {
        switch item {
        case .user, .divider: return true
        case .notice(let n): return n.code != "stopped" && n.code != "interrupted"
        default: return false
        }
    }

    // MARK: Live details

    private static let detailKeys = ["title", "description", "objective", "task", "task_name", "name", "question", "questions", "prompt", "message",
                                     "command", "cmd", "queries", "query", "pattern", "url", "uri", "file_path", "path", "target", "action", "status"]
    private static let detailMax = 160

    static func normalize(_ value: JSONValue?) -> String {
        var s = ""
        if let str = value?.string { s = str }
        else if let a = value?.array, a.allSatisfy({ $0.string != nil }) { s = a.compactMap(\.string).joined(separator: ", ") }
        s = s.replacingOccurrences(of: #"\s+"#, with: " ", options: .regularExpression).trimmingCharacters(in: .whitespaces)
        let chars = Array(s)
        if chars.count <= detailMax { return s }
        return String(chars.prefix(detailMax - 1)).trimmingTrailingWhitespace() + "…"
    }

    /// dsh `liveToolDetail`: what a running call works on (paths relative to the chat's folder).
    static func liveToolDetail(_ item: ToolItem, cwd: String) -> String {
        let args = item.args.object ?? [:]
        for key in detailKeys {
            guard var value = args[key] else { continue }
            if key == "questions", let qs = value.array {
                value = qs.first(where: { $0["question"]?.string != nil })?["question"] ?? .null
            }
            if key == "file_path" || key == "path", let s = value.string, !cwd.isEmpty, s.hasPrefix(cwd + "/") {
                value = .string(String(s.dropFirst(cwd.count + 1)))
            }
            let detail = normalize(value)
            if !detail.isEmpty { return detail }
        }
        // Only an unknown tool names itself; the others have their activity label.
        return Activity.of(item.name) == "tools" ? normalize(.string(item.name)) : ""
    }

    static func stripBold(_ s: String) -> String { s.replacingOccurrences(of: "**", with: "") }

    static func firstLine(_ text: String) -> String {
        if let i = text.firstIndex(of: "\n") { return String(text[..<i]) }
        return text
    }

    /// dsh: the streaming preview advances when a paragraph's first line is complete.
    static func latestCompletedParagraphFirstLine(_ text: String) -> String {
        let ns = text as NSString
        guard let separator = try? NSRegularExpression(pattern: #"\r?\n(?:[\t ]*\r?\n)+"#) else { return "" }
        var summary = ""
        var start = 0
        let matches = separator.matches(in: text, range: NSRange(location: 0, length: ns.length))
        var k = 0
        while true {
            let next: NSTextCheckingResult? = k < matches.count ? matches[k] : nil
            let end: Int
            if let next {
                let matched = ns.substring(with: next.range) as NSString
                end = next.range.location + matched.range(of: "\n").location
            } else {
                end = ns.length
            }
            let newline = ns.range(of: "\n", options: [], range: NSRange(location: start, length: ns.length - start)).location
            if newline != NSNotFound, newline <= end {
                let candidate = ns.substring(with: NSRange(location: start, length: newline - start)).trimmingCharacters(in: .whitespacesAndNewlines)
                if !candidate.isEmpty { summary = candidate }
            }
            guard let next else { return summary }
            start = next.range.location + next.range.length
            k += 1
        }
    }

    /// dsh `liveReasoningDetail`: the latest non-empty paragraph of streaming reasoning.
    static func reasoningDetail(_ text: String) -> String {
        let ns = text as NSString
        guard let separator = try? NSRegularExpression(pattern: #"\r?\n[\t ]*\r?\n"#) else { return "" }
        var paragraphs: [String] = []
        var last = 0
        for m in separator.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            paragraphs.append(ns.substring(with: NSRange(location: last, length: m.range.location - last)))
            last = m.range.location + m.range.length
        }
        paragraphs.append(ns.substring(from: last))
        for p in paragraphs.reversed() {
            let detail = normalize(.string(stripBold(p)))
            if !detail.isEmpty { return detail }
        }
        return ""
    }
}

extension String {
    func trimmingTrailingWhitespace() -> String {
        var s = Substring(self)
        while let last = s.last, last.isWhitespace { s = s.dropLast() }
        return String(s)
    }
}
