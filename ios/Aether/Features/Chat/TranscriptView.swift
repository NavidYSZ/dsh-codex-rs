import SwiftUI

/// The conversation: user bubbles, Markdown answers, work groups behind one status line, turn notices and the
/// live (streaming) tail of the running step. Follows new content while the reader is at the bottom.
struct TranscriptList: View {
    let chat: ChatModel
    let selection: ChatSelection
    @State private var pinned = true
    @State private var away = false

    private var running: Bool { chat.isRunning }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if let error = chat.loadError {
                        ContentUnavailableView {
                            Label(L.chatStatusError, systemImage: "exclamationmark.triangle")
                        } description: {
                            Text(L.chatLoadFailed(error))
                        } actions: {
                            Button(L.commonRetry) { Task { await chat.load() } }.buttonStyle(.borderedProminent)
                        }
                        .padding(.top, 40)
                    } else if !chat.loaded {
                        ProgressView(L.commonLoading).frame(maxWidth: .infinity).padding(.top, 40)
                    } else if chat.transcript.turns.isEmpty, chat.pending == nil, !running {
                        VStack(spacing: 10) {
                            AetherMark(size: 56)
                            Text(L.chatPlaceholderHero).font(.title3.weight(.semibold)).foregroundStyle(Theme.text)
                            Text(L.chatEmpty).font(.footnote).foregroundStyle(Theme.textMuted)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.top, 60)
                    }
                    let turns = chat.transcript.turns
                    let lastIndex = turns.count - 1
                    let liveTurn = running && lastIndex >= 0 && turns[lastIndex].end == nil ? lastIndex : -1
                    ForEach(Array(turns.enumerated()), id: \.element.turn) { i, turn in
                        TurnView(turn: turn, live: i == liveTurn, deltas: i == liveTurn ? chat.deltas : nil, cwd: chat.cwd, chat: chat, selection: selection)
                    }
                    if let p = chat.pending {
                        UserBubble(text: p.text, images: p.images, pending: true, client: chat.client)
                        if liveTurn < 0 { ThinkingRow() }
                    }
                    if running, liveTurn < 0, chat.pending == nil { ThinkingRow() }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, 14)
                .padding(.top, 12)
                .background(GeometryReader { g in
                    Color.clear.preference(key: BottomOffsetKey.self, value: g.frame(in: .named("scroll")).maxY)
                })
            }
            .coordinateSpace(name: "scroll")
            .scrollDismissesKeyboard(.interactively)
            .defaultScrollAnchor(.bottom)
            .onPreferenceChange(BottomOffsetKey.self) { maxY in
                // Only the reader scrolling up unpins; growing content never does.
                let atBottom = maxY <= viewportHeight + 80
                Task { @MainActor in
                    if atBottom { pinned = true }
                    else if maxY > viewportHeight + 200 { pinned = false }
                    away = !pinned
                }
            }
            .background(GeometryReader { g in Color.clear.onAppear { viewportHeight = g.size.height }.onChange(of: g.size.height) { _, h in viewportHeight = h } })
            .onChange(of: chat.events.count) { _, _ in if pinned { proxy.scrollTo("bottom", anchor: .bottom) } }
            .onChange(of: chat.deltas) { _, _ in if pinned { proxy.scrollTo("bottom", anchor: .bottom) } }
            .onChange(of: chat.pending) { _, _ in pinned = true; proxy.scrollTo("bottom", anchor: .bottom) }
            .onChange(of: chat.loaded) { _, _ in proxy.scrollTo("bottom", anchor: .bottom) }
            .overlay(alignment: .bottom) {
                if away {
                    Button {
                        pinned = true
                        withAnimation { proxy.scrollTo("bottom", anchor: .bottom) }
                    } label: {
                        Image(systemName: "arrow.down")
                            .font(.footnote.weight(.semibold))
                            .padding(10)
                            .background(Theme.surface, in: Circle())
                            .overlay(Circle().stroke(Theme.border))
                            .shadow(color: .black.opacity(0.08), radius: 6, y: 2)
                    }
                    .accessibilityLabel(L.chatScrollDown)
                    .padding(.bottom, 10)
                }
            }
            .refreshable { await chat.reconnect() }
        }
    }

    @State private var viewportHeight: CGFloat = 0
}

private struct BottomOffsetKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}

/// What the running step streams, folded into the turn's segments (ui/src/components/transcript.js `liveSegments`).
struct LiveReasoning: Hashable {
    var key: String
    var text: String
    var live: Bool
    var ms: Double?
}

enum LiveSegment: Identifiable {
    case item(TranscriptItem)
    case group(WorkGroup, live: [LiveReasoning], prep: String?, closed: Bool)
    case liveText(key: String, text: String, streaming: Bool)

    var id: String {
        switch self {
        case .item(let x): return x.key
        case .group(let g, _, _, _): return g.key
        case .liveText(let key, _, _): return key
        }
    }
}

private func liveSegments(_ turn: Turn, deltas: Deltas?) -> (segs: [LiveSegment], open: Bool, streaming: Bool) {
    var segs: [LiveSegment] = turn.segments.map {
        switch $0 {
        case .item(let x): return .item(x)
        case .group(let g): return .group(g, live: [], prep: nil, closed: g.closed)
        }
    }
    var groups = turn.groups
    var openIndex: Int?
    if let last = segs.indices.last, case .group(let g, _, _, _) = segs[last] {
        segs[last] = .group(g, live: [], prep: nil, closed: false)
        openIndex = last
    }
    let blocks = deltas?.turn == turn.turn ? (deltas?.sortedBlocks ?? []) : []
    var streaming = false
    for (k, entry) in blocks.enumerated() {
        let b = entry.block
        let next = k + 1 < blocks.count ? blocks[k + 1].block : nil
        if b.type == "text" {
            if b.text.isEmpty { continue }
            if let i = openIndex, case .group(let g, let live, let prep, _) = segs[i] { segs[i] = .group(g, live: live, prep: prep, closed: true) }
            openIndex = nil
            streaming = true
            segs.append(.liveText(key: "live-t-\(deltas?.key ?? "")-\(entry.index)", text: b.text, streaming: next == nil))
            continue
        }
        if b.type == "reasoning", b.text.isEmpty { continue }
        if openIndex == nil {
            segs.append(.group(WorkGroup(key: "g:\(groups)", items: [], closed: false, counts: []), live: [], prep: nil, closed: false))
            groups += 1
            openIndex = segs.count - 1
        }
        guard let i = openIndex, case .group(let g, var live, var prep, let closed) = segs[i] else { continue }
        // Reasoning stays "Thinking" only until the next block starts streaming.
        if b.type == "reasoning" {
            live.append(LiveReasoning(key: "live-r-\(deltas?.key ?? "")-\(entry.index)", text: b.text, live: next == nil, ms: next.map { $0.start - b.start }))
        } else if b.type == "tool-call" {
            prep = b.text
        }
        segs[i] = .group(g, live: live, prep: prep, closed: closed)
    }
    return (segs, openIndex != nil, streaming)
}

private let sourceCodes: Set<String> = ["goal", "schedule", "team-message", "job", "mcp-activation", "mcp-deactivation"]

/// One turn in the Standard view: work groups behind status lines, and once it is done, folded under one turn line.
struct TurnView: View {
    let turn: Turn
    let live: Bool
    let deltas: Deltas?
    let cwd: String
    let chat: ChatModel
    let selection: ChatSelection
    @State private var unfolded = false

    var body: some View {
        let work = turn.work
        let ended = !live && turn.end != nil
        let waiting = turn.items.contains { $0.tool?.isWaiting ?? false }
        let alwaysOpen = !ended || ["aborted", "error", "interrupted"].contains(turn.status) || work.steered || waiting
        let foldable = ended && !alwaysOpen && work.hidden
        let folded = foldable && !unfolded
        let (segs, open, streaming) = live ? liveSegments(turn, deltas: deltas)
            : (turn.segments.map { s -> LiveSegment in
                switch s {
                case .item(let x): return .item(x)
                case .group(let g): return .group(g, live: [], prep: nil, closed: true)
                }
            }, false, false)
        let compacting = turn.items.contains { if case .divider(let d) = $0 { return d.running } else { return false } }

        VStack(alignment: .leading, spacing: 12) {
            let lineAt = firstLineIndex(segs)
            ForEach(Array(segs.enumerated()), id: \.element.id) { i, seg in
                if ended, i == lineAt {
                    TurnLine(turn: turn, folded: folded, foldable: foldable, cost: chat.stats?.turns.first { $0.turn == turn.turn }) {
                        withAnimation(.snappy) { unfolded.toggle() }
                    }
                }
                switch seg {
                case .liveText(_, let text, _):
                    MarkdownView(text: text)
                case .item(let x):
                    if !(folded && x.key != work.answerKey && !Worklog.staysVisible(x)) {
                        render(x, ended: ended, answerKey: work.answerKey)
                    }
                case .group(let g, let liveReasoning, let prep, let closed):
                    if !folded {
                        WorkGroupView(group: g, closed: closed || !live, live: liveReasoning, prep: prep, cwd: cwd, client: chat.client)
                    }
                }
            }
            if ended, lineAt == nil {
                TurnLine(turn: turn, folded: folded, foldable: foldable, cost: chat.stats?.turns.first { $0.turn == turn.turn }) {
                    withAnimation(.snappy) { unfolded.toggle() }
                }
            }
            if ended, turn.status == "completed" { ChangesCard(turn: turn, cwd: cwd) }
            if live, let retry = turn.retry { RetryView(retry: retry) }
            if live, !streaming, !open, turn.retry == nil, !compacting { ThinkingRow() }
        }
    }

    /// The turn line sits under the prompt (and a goal, reminder or team line).
    private func firstLineIndex(_ segs: [LiveSegment]) -> Int? {
        for (i, seg) in segs.enumerated() {
            switch seg {
            case .item(let x):
                if case .user = x { continue }
                if case .notice(let n) = x, sourceCodes.contains(n.code) { continue }
                return i
            case .group, .liveText:
                return i
            }
        }
        return nil
    }

    @ViewBuilder
    private func render(_ x: TranscriptItem, ended: Bool, answerKey: String?) -> some View {
        switch x {
        case .user(let u):
            UserBubble(text: u.text, images: u.images, pending: false, client: chat.client)
        case .text(let t):
            if ended, t.key == answerKey {
                AnswerView(text: t.text, chat: chat, turn: turn, selection: selection)
            } else {
                MarkdownView(text: t.text)
            }
        case .notice(let n):
            if n.code == "error" { FailureNotice(item: n, turn: turn, chat: chat, selection: selection) }
            else if n.code == "maxTokens" { MaxTokensNotice(turn: turn, chat: chat, selection: selection) }
            else { NoticeView(item: n) }
        case .divider(let d):
            CompactionView(item: d)
        case .reasoning, .tool:
            EmptyView()
        }
    }
}

// MARK: - Work groups

private func iconOf(kind: String) -> String {
    if kind.hasPrefix("mcp:") { return "powerplug" }
    switch kind {
    case "thinking": return "brain"
    case "read", "write": return "doc.text"
    case "readImage": return "photo"
    case "search": return "magnifyingglass"
    case "edit": return "pencil"
    case "commands", "code": return "terminal"
    case "webSearch", "webFetch": return "globe"
    case "subagents": return "person.2"
    case "plan": return "list.bullet"
    case "questions": return "questionmark.circle"
    case "browser": return "macwindow"
    case "mac": return "cursorarrow"
    case "skill": return "book"
    default: return "wrench.and.screwdriver"
    }
}

private struct LiveActivity {
    var activity: String
    var detail: String
    var preparing: Bool
    var waiting: Bool
}

/// What an open group works on right now (dsh `processActivity` for the live part).
private func liveActivity(_ group: WorkGroup, live: [LiveReasoning], prep: String?, cwd: String) -> LiveActivity {
    if let prep { return LiveActivity(activity: Activity.of(prep), detail: "", preparing: true, waiting: false) }
    var running: ToolItem?
    for case .tool(let t) in group.items where t.result == nil {
        if running == nil || t.time >= (running?.time ?? 0) { running = t }
    }
    if let running {
        let waiting = running.isWaiting
        return LiveActivity(activity: waiting ? "questions" : Activity.of(running.name), detail: Worklog.liveToolDetail(running, cwd: cwd), preparing: false, waiting: waiting)
    }
    let text = live.last?.text ?? ""
    return LiveActivity(activity: "thinking", detail: text.isEmpty ? "" : Worklog.reasoningDetail(text), preparing: false, waiting: false)
}

/// A run of reasoning and tool steps behind one status line. Pending approvals keep it open.
struct WorkGroupView: View {
    let group: WorkGroup
    let closed: Bool
    let live: [LiveReasoning]
    let prep: String?
    let cwd: String
    let client: any ServerAPI
    @State private var open = false

    var body: some View {
        let waiting = group.items.contains { $0.tool?.isWaiting ?? false }
        let expanded = open || waiting
        let act = closed ? LiveActivity(activity: "thinking", detail: "", preparing: false, waiting: false) : liveActivity(group, live: live, prep: prep, cwd: cwd)
        let label: String = {
            if closed { return Activity.processTitle(group.counts) }
            if act.waiting { return L.chatWaitingYou }
            if act.preparing { return Activity.label(.prepare, act.activity == "thinking" ? "tools" : act.activity) }
            return Activity.label(.live, act.activity)
        }()
        let icon = iconOf(kind: closed ? (group.counts.first?.kind ?? "thinking") : act.activity)
        let empty = group.items.isEmpty && live.isEmpty

        VStack(alignment: .leading, spacing: 6) {
            Button {
                guard !empty else { return }
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: icon).font(.footnote).foregroundStyle(Theme.textMuted).frame(width: 16)
                    ShimmerText(text: act.detail.isEmpty || closed ? label : "\(label) · \(act.detail)", active: !closed)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if !empty {
                        Image(systemName: "chevron.right").font(.caption2.weight(.semibold)).foregroundStyle(Theme.textMuted)
                            .rotationEffect(.degrees(expanded ? 90 : 0))
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(empty)
            .accessibilityLabel(label)
            .accessibilityValue(expanded ? L.chatA11yCollapse : L.chatA11yExpand)
            if expanded {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(group.items) { x in
                        switch x {
                        case .tool(let t): ToolRowView(item: t, cwd: cwd, client: client)
                        case .reasoning(let r): ReasoningLine(text: r.text, ms: r.ms, live: false)
                        default: EmptyView()
                        }
                    }
                    ForEach(live, id: \.key) { b in ReasoningLine(text: b.text, ms: b.ms, live: b.live) }
                }
                .padding(.leading, 24)
            }
        }
    }
}

/// Reasoning as one line: while it streams, the latest finished paragraph's first line, afterwards its first line;
/// a tap shows the whole text.
struct ReasoningLine: View {
    let text: String
    let ms: Double?
    let live: Bool
    @State private var open = false

    var body: some View {
        let summary = Worklog.stripBold(live ? Worklog.latestCompletedParagraphFirstLine(text) : Worklog.firstLine(text)).trimmingCharacters(in: .whitespaces)
        let label = live ? L.chatThinking : (ms.map { $0 >= 500 } ?? false) ? L.chatThoughtFor(Format.duration(ms: ms ?? 0)) : L.chatThought
        VStack(alignment: .leading, spacing: 6) {
            Button { withAnimation(.snappy) { open.toggle() } } label: {
                HStack(spacing: 8) {
                    Image(systemName: "brain").font(.footnote).foregroundStyle(Theme.textMuted).frame(width: 16)
                    ShimmerText(text: label, active: live)
                    if !open, !summary.isEmpty {
                        Text("·").foregroundStyle(Theme.textMuted)
                        Text(summary).font(.footnote).foregroundStyle(Theme.textMuted).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right").font(.caption2.weight(.semibold)).foregroundStyle(Theme.textMuted)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(label)
            if open {
                MarkdownView(text: text, muted: true)
                    .padding(.leading, 24)
            }
        }
    }
}

/// "Analysiert die Anfrage" before anything streams.
struct ThinkingRow: View {
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "brain").font(.footnote).foregroundStyle(Theme.textMuted).frame(width: 16)
            ShimmerText(text: L.workLiveThinking, active: true)
        }
        .accessibilityLabel(L.workLiveThinking)
    }
}

/// Text with a moving highlight while work is in progress (Codex "shimmer").
struct ShimmerText: View {
    let text: String
    let active: Bool
    @State private var phase: CGFloat = -1

    var body: some View {
        Text(text)
            .font(.footnote.weight(.medium))
            .foregroundStyle(Theme.textMuted)
            .overlay {
                if active {
                    GeometryReader { g in
                        LinearGradient(colors: [.clear, Theme.text.opacity(0.9), .clear], startPoint: .leading, endPoint: .trailing)
                            .frame(width: g.size.width * 0.6)
                            .offset(x: phase * g.size.width)
                            .mask(Text(text).font(.footnote.weight(.medium)))
                    }
                    .allowsHitTesting(false)
                }
            }
            .onAppear { if active { animate() } }
            .onChange(of: active) { _, on in if on { animate() } }
    }

    private func animate() {
        phase = -1
        withAnimation(.linear(duration: 1.4).repeatForever(autoreverses: false)) { phase = 1.2 }
    }
}

/// "Fertig in 1 min 20 s · 12 Tool-Aufrufe · 3 Nachrichten · $0.0031"; a tap folds the turn.
struct TurnLine: View {
    let turn: Turn
    let folded: Bool
    let foldable: Bool
    let cost: SessionStats.TurnUsage?
    var onToggle: () -> Void

    var body: some View {
        let work = turn.work
        var parts: [String] = []
        let label: String = {
            if turn.status == "aborted" { return L.chatStopped }
            if turn.status == "error" { return L.chatTurnFailedShort }
            if let s = turn.start, let e = turn.end { return L.chatTurnTook(Format.duration(ms: max(1000, e - s))) }
            return L.chatTurnWorked
        }()
        let _ = parts.append(label)
        let _ = work.toolCalls > 0 ? parts.append(L.chatToolCalls(work.toolCalls)) : ()
        let _ = work.messages > 0 ? parts.append(L.chatMessagesN(work.messages)) : ()
        let _ = (turn.todos?.isEmpty == false) ? parts.append(L.chatPlanShort(turn.todos!.filter { $0.status == "completed" }.count, turn.todos!.count)) : ()
        let _ = (cost.map { $0.costUsd > 0 } ?? false) ? parts.append(Format.usd(cost!.costUsd)) : ()
        Button(action: onToggle) {
            HStack(spacing: 6) {
                Text(parts.joined(separator: " · ")).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(1)
                if foldable {
                    Image(systemName: "chevron.down").font(.caption2.weight(.semibold)).foregroundStyle(Theme.textMuted)
                        .rotationEffect(.degrees(folded ? 0 : 180))
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!foldable)
        .accessibilityLabel(parts.joined(separator: ", "))
        .accessibilityValue(foldable ? (folded ? L.chatA11yExpand : L.chatA11yCollapse) : "")
    }
}

/// "N Dateien geändert" under a finished turn, from its edit/write rows.
struct ChangesCard: View {
    let turn: Turn
    let cwd: String

    private struct Change: Identifiable {
        var path: String
        var add: Int
        var del: Int
        var id: String { path }
    }

    private var changes: [Change] {
        var out: [String: Change] = [:]
        var order: [String] = []
        for case .tool(let t) in turn.items where (t.name == "edit" || t.name == "write") && !(t.result?.isError ?? true) {
            let path = ToolRows.editPath(t)
            guard !path.isEmpty else { continue }
            let counts = Diff.countChanges(ToolRows.editHunks(t).flatMap { $0 })
            if out[path] == nil { order.append(path); out[path] = Change(path: path, add: 0, del: 0) }
            out[path]?.add += counts.add
            out[path]?.del += counts.del
        }
        return order.compactMap { out[$0] }
    }

    var body: some View {
        let list = changes
        if !list.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(L.toolFiles(list.count)).font(.caption.weight(.semibold)).foregroundStyle(Theme.text)
                    Counts(add: list.reduce(0) { $0 + $1.add }, del: list.reduce(0) { $0 + $1.del })
                }
                ForEach(list.prefix(6)) { c in
                    HStack {
                        Image(systemName: "doc.text").font(.caption2).foregroundStyle(Theme.textMuted)
                        Text(Format.relPath(c.path, cwd: cwd)).font(.caption.monospaced()).lineLimit(1).truncationMode(.middle)
                        Spacer()
                        Counts(add: c.add, del: c.del)
                    }
                }
            }
            .padding(10)
            .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
        }
    }
}

struct Counts: View {
    var add: Int
    var del: Int

    var body: some View {
        HStack(spacing: 6) {
            if add > 0 { Text("+\(add)").font(.caption.monospacedDigit()).foregroundStyle(Theme.online) }
            if del > 0 { Text("−\(del)").font(.caption.monospacedDigit()).foregroundStyle(Theme.danger) }
        }
    }
}

struct RetryView: View {
    let retry: RetryInfo
    @State private var now = Date()
    private let timer = Timer.publish(every: 0.5, on: .main, in: .common).autoconnect()

    var body: some View {
        let left = max(0, retry.time + retry.delayMs - now.timeIntervalSince1970 * 1000)
        let why = retry.failure?["message"]?.string ?? ""
        Text(L.chatRetrying(retry.retry, retry.max.map(String.init) ?? "∞", Format.duration(ms: max(left, 1000))) + (why.isEmpty ? "" : " · \(why)"))
            .font(.footnote)
            .foregroundStyle(Theme.warning)
            .onReceive(timer) { now = $0 }
    }
}
