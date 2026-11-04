import Foundation

// Port of ui/src/reduce.js: session events (dsh format v4 envelopes) → what the conversation shows.
// Only the events the UI needs are folded; everything else (request/header, inbox splices, delivery
// receipts, unknown plugin events) is ignored, so a log written by the official dsh renders too.

struct Todo: Codable, Hashable, Sendable {
    var content: String
    var status: String

    init(content: String, status: String) {
        self.content = content
        self.status = status
    }

    init?(json: JSONValue) {
        guard let content = json["content"]?.string else { return nil }
        self.content = content
        self.status = json["status"]?.string ?? "pending"
    }
}

struct ApprovalInfo: Hashable, Sendable {
    var id: String
    var toolName: String
    var callId: String?
    var reason: String
    var outcome: String?
    var time: Double

    var isPending: Bool { outcome == nil }
    var isDenied: Bool { outcome == "rejected" || outcome == "cancelled" || outcome == "unavailable" }
}

struct UserItem: Hashable, Sendable {
    var key: String
    var text: String
    var time: Double
    var images: [ImageAttachmentRef]
}

struct ReasoningItem: Hashable, Sendable {
    var key: String
    var text: String
    /// How long the model spent in the block, from the timed stream record.
    var ms: Double?
}

struct TextItem: Hashable, Sendable {
    var key: String
    var text: String
}

struct ToolResult: Hashable, Sendable {
    var content: [JSONValue]
    var isError: Bool
    var error: JSONValue?
    var meta: JSONValue?
    var time: Double

    var text: String { content.filter { $0["type"]?.string == "text" }.compactMap { $0["text"]?.string }.joined(separator: "\n") }
    var image: ImageAttachmentRef? {
        content.first(where: { $0["type"]?.string == "image" })?["attachment"].flatMap { $0.isNull ? nil : ImageAttachmentRef(raw: $0) }
    }
}

struct ScreenSize: Hashable, Sendable {
    var width: Double
    var height: Double

    init?(json: JSONValue?) {
        guard let w = json?["width"]?.double, let h = json?["height"]?.double, w > 0, h > 0 else { return nil }
        width = w
        height = h
    }
}

struct Point: Hashable, Sendable {
    var x: Double
    var y: Double
}

/// Where the previous step of the same Mac tool left its screenshot and cursor.
struct Shot: Hashable, Sendable {
    var image: ImageAttachmentRef
    var screen: ScreenSize
    var place: String?
    var cursor: Point?
}

struct ToolItem: Hashable, Sendable {
    var key: String
    var id: String
    var name: String
    var argsRaw: String
    var args: JSONValue
    var result: ToolResult?
    var approval: ApprovalInfo?
    var todos: [Todo]?
    var time: Double
    var prevShot: Shot?
    var latestTodo = false

    var isRunning: Bool { result == nil }
    var isWaiting: Bool { approval?.isPending ?? false }
    var failed: Bool { result?.isError ?? false }
}

struct NoticeItem: Hashable, Sendable {
    var key: String
    var code: String
    var tone: String
    var round: Int?
    var max: String?
    var prompt: String?
    var sender: String?
    var text: String?
    var server: String?
    var failure: JSONValue?
    var provider: String?
}

struct DividerItem: Hashable, Sendable {
    var key: String
    var code = "compacted"
    var running: Bool
    var summary: String?
    var items: Int?
    var tokens: Int?
    var error: String?
}

enum TranscriptItem: Hashable, Sendable, Identifiable {
    case user(UserItem)
    case reasoning(ReasoningItem)
    case text(TextItem)
    case tool(ToolItem)
    case notice(NoticeItem)
    case divider(DividerItem)

    var key: String {
        switch self {
        case .user(let x): return x.key
        case .reasoning(let x): return x.key
        case .text(let x): return x.key
        case .tool(let x): return x.key
        case .notice(let x): return x.key
        case .divider(let x): return x.key
        }
    }

    var id: String { key }
    var isText: Bool { if case .text = self { return true } else { return false } }
    var isReasoning: Bool { if case .reasoning = self { return true } else { return false } }
    var isTool: Bool { if case .tool = self { return true } else { return false } }
    var tool: ToolItem? { if case .tool(let t) = self { return t } else { return nil } }
}

struct RetryInfo: Hashable, Sendable {
    var retry: Int
    var max: Int?
    var delayMs: Double
    var failure: JSONValue?
    var time: Double
}

struct Turn: Hashable, Sendable, Identifiable {
    var turn: Int
    var items: [TranscriptItem]
    var status: String
    var reason: JSONValue?
    var start: Double?
    var end: Double?
    var retry: RetryInfo?
    var todos: [Todo]?
    var segments: [Segment] = []
    var groups = 0
    var work = TurnWork(answerKey: nil, toolCalls: 0, messages: 0, steered: false, hidden: false)

    var id: Int { turn }
    var ended: Bool { end != nil }
}

struct Transcript: Hashable, Sendable {
    var turns: [Turn] = []
    var title = ""
    var todos: [Todo]?
    /// The turn that wrote the latest to-do list.
    var todosTurn: Int?
    var model: String?
    var effort: String?
    var sandbox: String?
    var contextWindow = 0
    var pendingApproval: ApprovalInfo?

    static let empty = Transcript()
}

enum Reduce {
    static let codexTools: Set<String> = ["computer", "app", "browser"]

    /// The picker's model reference: bare DeepSeek ids, `provider/model` otherwise.
    static func modelRef(provider: String?, model: String) -> String {
        if let provider, !provider.isEmpty, provider != "deepseek-official" { return "\(provider)/\(model)" }
        return model
    }

    /// Tool arguments: an object as is, a JSON string parsed, anything else `{}`.
    static func parseArgs(_ raw: JSONValue?) -> JSONValue {
        guard let raw else { return .object([:]) }
        if case .object = raw { return raw }
        if let s = raw.string, !s.isEmpty, let parsed = JSONValue.parse(s), !parsed.isNull { return parsed }
        return .object([:])
    }

    static func argsText(_ raw: JSONValue?) -> String {
        guard let raw else { return "" }
        if let s = raw.string { return s }
        if case .object = raw { return raw.encoded() }
        return ""
    }

    private static func textOf(_ content: JSONValue?) -> String {
        (content?.array ?? []).filter { $0["type"]?.string == "text" }.compactMap { $0["text"]?.string }.joined(separator: "\n")
    }

    /// Per content index: how long the model spent in that block (from the timed stream record).
    private static func blockTimes(_ stream: JSONValue?) -> [Int: Double] {
        var start: [Int: Double] = [:]
        var end: [Int: Double] = [:]
        for rec in stream?.array ?? [] {
            guard rec["type"]?.string == "chunk", let c = rec["chunk"], let index = c["index"]?.int, let time = rec["time"]?.double else { continue }
            if c["type"]?.string == "block-start" { start[index] = time }
            else if c["type"]?.string == "block-end", start[index] != nil { end[index] = time }
        }
        var ms: [Int: Double] = [:]
        for (i, s) in start { if let e = end[i], e >= s { ms[i] = e - s } }
        return ms
    }

    // Mutable boxes while folding; tool rows, approvals and compaction markers change after they were placed.
    private final class ToolBox {
        var item: ToolItem
        var approval: ApprovalBox?
        init(_ item: ToolItem) { self.item = item }
    }

    private final class ApprovalBox {
        var info: ApprovalInfo
        init(_ info: ApprovalInfo) { self.info = info }
    }

    private final class DividerBox {
        var item: DividerItem
        init(_ item: DividerItem) { self.item = item }
    }

    private enum Entry {
        case value(TranscriptItem)
        case tool(ToolBox)
        case divider(DividerBox)
    }

    private final class TurnBox {
        var turn: Int
        var items: [Entry] = []
        var status = "running"
        var reason: JSONValue?
        var start: Double?
        var end: Double?
        var retry: RetryInfo?
        var todos: [Todo]?
        init(turn: Int, start: Double?) { self.turn = turn; self.start = start }
    }

    /// The line for a turn that a goal round, a reminder or a teammate started.
    private static func sourceNotice(kind: String, data d: JSONValue, seq: Int) -> NoticeItem {
        let text = textOf(d["content"])
        var n = NoticeItem(key: "n:\(seq)", code: kind, tone: "muted")
        switch kind {
        case "goal":
            if let r = text.range(of: #"Round: \d+/(\d+)"#, options: .regularExpression) {
                let m = text[r]
                if let slash = m.lastIndex(of: "/") { n.max = String(m[m.index(after: slash)...]) }
            }
            n.round = d["source"]?["round"]?.int
        case "schedule":
            // dsh frames reminders as JSON strings: one prompt or a batch.
            var prompt = ""
            if let r = text.range(of: #"reminder_prompt_json: (".*")"#, options: .regularExpression) {
                let json = String(text[r].dropFirst("reminder_prompt_json: ".count))
                prompt = JSONValue.parse(json)?.string ?? ""
            }
            if prompt.isEmpty, let r = text.range(of: #"reminders_json: (\[.*\])"#, options: [.regularExpression]) {
                let json = String(text[r].dropFirst("reminders_json: ".count))
                prompt = (JSONValue.parse(json)?.array ?? []).compactMap { $0["reminder_prompt"]?.string }.joined(separator: " · ")
            }
            n.prompt = prompt
        default:
            n.sender = d["source"]?["senderName"]?.string ?? ""
            n.text = text.replacingOccurrences(of: #"^Team message \S+ from [^:]*:\s*"#, with: "", options: .regularExpression)
        }
        return n
    }

    /// Folds the log into turns. Each turn holds ordered items: user | reasoning | text | tool | notice | divider.
    static func buildTranscript(_ events: [SessionEvent]) -> Transcript {
        var out = Transcript()
        var turns: [TurnBox] = []
        var calls: [String: ToolBox] = [:]
        var approvals: [String: ApprovalBox] = [:]
        var compactions: [String: DividerBox] = [:]
        var shots: [String: Shot] = [:]
        var turn: TurnBox?
        var lastTodoCall: ToolBox?
        // The provider of the latest request: a failure notice links to its settings.
        var provider: String?

        func openTurn(_ n: Int, _ time: Double?) -> TurnBox {
            let t = TurnBox(turn: n, start: time)
            turns.append(t)
            turn = t
            return t
        }
        func current(_ n: Int?, _ time: Double?) -> TurnBox {
            if let t = turn, n == nil || t.turn == n { return t }
            return openTurn(n ?? turns.count + 1, time)
        }
        func toolItem(_ id: String, _ name: String, _ argsRaw: JSONValue?, _ time: Double) -> ToolBox {
            let item = ToolItem(key: "c:\(id)", id: id, name: name, argsRaw: argsText(argsRaw), args: parseArgs(argsRaw), result: nil,
                                approval: nil, todos: nil, time: time, prevShot: codexTools.contains(name) ? shots[name] : nil)
            let box = ToolBox(item)
            calls[id] = box
            if name == "todo_write" { lastTodoCall = box }
            return box
        }

        for e in events {
            let d = e.data
            switch e.type {
            case "turn/start":
                _ = openTurn(d["turn"]?.int ?? turns.count + 1, e.time)

            case "user/message":
                let kind = d["source"]?["kind"]?.string
                if kind == "goal" || kind == "schedule" || kind == "team-message" {
                    current(nil, e.time).items.append(.value(.notice(sourceNotice(kind: kind!, data: d, seq: e.seq))))
                    break
                }
                if kind == "mcp-activation" || kind == "mcp-deactivation" {
                    current(nil, e.time).items.append(.value(.notice(NoticeItem(key: "m:\(e.seq)", code: kind!, tone: "muted", server: d["source"]?["server"]?.string ?? ""))))
                    break
                }
                if kind == "tool-jobs" {
                    current(nil, e.time).items.append(.value(.notice(NoticeItem(key: "j:\(e.seq)", code: "job", tone: "muted", text: d["source"]?["summary"]?.string ?? textOf(d["content"])))))
                    break
                }
                // Runtime context, AGENTS.md, skills etc. are model-only user messages.
                guard kind == "user" else { break }
                let images = (d["content"]?.array ?? []).filter { $0["type"]?.string == "image" && $0["attachment"] != nil && !($0["attachment"]!.isNull) }
                    .map { ImageAttachmentRef(raw: $0["attachment"]!) }
                current(nil, e.time).items.append(.value(.user(UserItem(key: "u:\(e.seq)", text: textOf(d["content"]), time: e.time, images: images))))

            case "assistant/message":
                let t = current(d["turn"]?.int, e.time)
                let ms = blockTimes(d["stream"])
                let msg = d["message"] ?? .object([:])
                for (i, b) in (msg["content"]?.array ?? []).enumerated() {
                    let type = b["type"]?.string
                    if type == "reasoning" {
                        let text = b["text"]?.string ?? ""
                        if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            t.items.append(.value(.reasoning(ReasoningItem(key: "r:\(e.seq):\(i)", text: text, ms: ms[i]))))
                        }
                    } else if type == "text" {
                        let text = b["text"]?.string ?? ""
                        if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            t.items.append(.value(.text(TextItem(key: "t:\(e.seq):\(i)", text: text))))
                        }
                    } else if type == "tool-call" {
                        let id = b["id"]?.string ?? ""
                        t.items.append(.tool(calls[id] ?? toolItem(id, b["name"]?.string ?? "tool", b["arguments"], e.time)))
                    }
                }
                if d["interrupted"]?.truthy == true {
                    t.items.append(.value(.notice(NoticeItem(key: "i:\(e.seq)", code: "stopped", tone: "muted"))))
                }
                if let model = msg["source"]?["model"]?.string { out.model = modelRef(provider: msg["source"]?["provider"]?.string, model: model) }
                provider = msg["source"]?["provider"]?.string ?? provider
                t.retry = nil

            case "tool/call":
                let id = d["callId"]?.string ?? ""
                if calls[id] != nil { break }
                current(d["turn"]?.int, e.time).items.append(.tool(toolItem(id, d["name"]?.string ?? "tool", d["arguments"], e.time)))

            case "tool/result":
                let m = d["message"] ?? .object([:])
                let id = m["toolCallId"]?.string ?? m["source"]?["callId"]?.string ?? ""
                var box = calls[id]
                if box == nil {
                    let b = toolItem(id, d["meta"]?["tool"]?.string ?? "tool", .string("{}"), e.time)
                    current(d["turn"]?.int, e.time).items.append(.tool(b))
                    box = b
                }
                guard let box else { break }
                let result = ToolResult(content: m["content"]?.array ?? [], isError: m["isError"]?.truthy ?? false,
                                        error: d["error"].flatMap { $0.isNull ? nil : $0 }, meta: d["meta"].flatMap { $0.isNull ? nil : $0 }, time: e.time)
                box.item.result = result
                if codexTools.contains(box.item.name), let image = result.image, let screen = ScreenSize(json: result.meta?["screen"]) {
                    let meta = result.meta ?? .object([:])
                    var cursor: Point?
                    if let cx = meta["cursor"]?["x"]?.double, let cy = meta["cursor"]?["y"]?.double { cursor = Point(x: cx, y: cy) }
                    else if let x = box.item.args["x"]?.double, box.item.args["x"]?.isNumber == true { cursor = Point(x: x, y: box.item.args["y"]?.double ?? 0) }
                    shots[box.item.name] = Shot(image: image, screen: screen, place: meta["place"]?.string, cursor: cursor)
                }

            case "todo/write":
                let todos = (d["todos"]?.array ?? []).compactMap(Todo.init(json:))
                out.todos = todos
                if let last = lastTodoCall, last.item.todos == nil { last.item.todos = todos }
                let tn = current(d["turn"]?.int, e.time)
                tn.todos = todos
                out.todosTurn = tn.turn

            case "approval/asked":
                let id = d["id"]?.string ?? ""
                let a = ApprovalBox(ApprovalInfo(id: id, toolName: d["toolName"]?.string ?? "", callId: d["callId"]?.string, reason: d["reason"]?.string ?? "", outcome: nil, time: e.time))
                approvals[id] = a
                if let callId = d["callId"]?.string, let item = calls[callId] { item.approval = a }
                out.pendingApproval = a.info

            case "approval/decided":
                let id = d["id"]?.string ?? ""
                if let a = approvals[id] { a.info.outcome = d["outcome"]?.string }
                if out.pendingApproval?.id == id { out.pendingApproval = nil }

            case "llm/retry":
                provider = d["provider"]?.string ?? provider
                current(d["turn"]?.int, e.time).retry = RetryInfo(retry: d["retry"]?.int ?? 0, max: d["maxRetries"]?.int, delayMs: d["delayMs"]?.double ?? 0,
                                                                   failure: d["failure"].flatMap { $0.isNull ? nil : $0 }, time: e.time)

            // dsh `compactSummary`: one marker per compaction, running until its end, with the summary and how
            // much it replaced. A manual /compact runs between turns (`turn: null`): its marker closes the last turn.
            case "compaction/start":
                let box = DividerBox(DividerItem(key: "k:\(e.seq)", running: true))
                compactions[d["compactionId"]?.string ?? ""] = box
                let host = (turn == nil && d["turn"]?.int == nil && !turns.isEmpty) ? turns[turns.count - 1] : current(nil, e.time)
                host.items.append(.divider(box))

            case "compaction/summary":
                guard let box = compactions[d["compactionId"]?.string ?? ""] else { break }
                let text = (d["summary"]?.array ?? []).map { $0["type"]?.string == "text" ? ($0["text"]?.string ?? "") : "" }.joined()
                box.item.summary = text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : text
                if let seqs = d["shadowedSeqs"]?.array { box.item.items = seqs.count }
                if let tokens = d["shadowedTokenCount"]?.int { box.item.tokens = tokens }

            case "compaction/end":
                var box = compactions[d["compactionId"]?.string ?? ""]
                if box == nil {
                    let b = DividerBox(DividerItem(key: "k:\(e.seq)", running: false))
                    current(nil, e.time).items.append(.divider(b))
                    box = b
                }
                box?.item.running = false
                box?.item.error = d["error"]?.string

            case "turn/end":
                let t = current(d["turn"]?.int, e.time)
                let kind = d["reason"]?["kind"]?.string ?? "completed"
                t.status = kind
                t.reason = d["reason"].flatMap { $0.isNull ? nil : $0 }
                t.end = e.time
                t.retry = nil
                switch kind {
                case "error":
                    t.items.append(.value(.notice(NoticeItem(key: "x:\(e.seq)", code: "error", tone: "error",
                                                             failure: d["reason"]?["error"].flatMap { $0.isNull ? nil : $0 }, provider: provider))))
                case "aborted": t.items.append(.value(.notice(NoticeItem(key: "x:\(e.seq)", code: "stopped", tone: "muted"))))
                case "max-tokens": t.items.append(.value(.notice(NoticeItem(key: "x:\(e.seq)", code: "maxTokens", tone: "warn"))))
                case "interrupted": t.items.append(.value(.notice(NoticeItem(key: "x:\(e.seq)", code: "interrupted", tone: "warn"))))
                default: break
                }
                // An approval still open when the turn ended will never be answered.
                out.pendingApproval = nil
                turn = nil

            case "session/title":
                if let title = d["title"]?.string, !title.isEmpty { out.title = title }

            case "model/selection":
                if let model = d["model"]?.string, !model.isEmpty { out.model = modelRef(provider: d["provider"]?.string, model: model) }
                out.effort = d["reasoningEffort"]?.string

            case "request/header":
                if let c = d["header"]?["config"] {
                    if let model = c["model"]?.string, !model.isEmpty { out.model = modelRef(provider: c["provider"]?.string, model: model) }
                    if let o = c.object, o["reasoningEffort"] != nil { out.effort = c["reasoningEffort"]?.string }
                }

            case "request/context":
                if let w = d["contextWindow"]?.int, w != 0 { out.contextWindow = w }
                if let model = d["model"]?.string, !model.isEmpty { out.model = modelRef(provider: d["provider"]?.string, model: model) }
                provider = d["provider"]?.string ?? provider

            case "sandbox/mode":
                out.sandbox = d["mode"]?.string ?? out.sandbox

            default:
                break
            }
        }
        if let last = lastTodoCall { last.item.latestTodo = true }

        // Materialise: shared boxes become values; work groups and the fold once per event, not per render.
        out.turns = turns.map { t in
            let items: [TranscriptItem] = t.items.map { entry in
                switch entry {
                case .value(let v): return v
                case .tool(let box):
                    var item = box.item
                    item.approval = box.approval?.info
                    return .tool(item)
                case .divider(let box): return .divider(box.item)
                }
            }
            var turn = Turn(turn: t.turn, items: items, status: t.status, reason: t.reason, start: t.start, end: t.end, retry: t.retry, todos: t.todos)
            let grouped = Worklog.groupItems(items, ended: t.end != nil)
            turn.segments = grouped.segments
            turn.groups = grouped.groups
            turn.work = Worklog.turnWork(items)
            return turn
        }
        return out
    }
}
