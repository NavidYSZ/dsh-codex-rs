import SwiftUI

/// The user's prompt, right-aligned, with its images.
struct UserBubble: View {
    let text: String
    let images: [ImageAttachmentRef]
    let pending: Bool
    let client: any ServerAPI

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            if !images.isEmpty {
                HStack(spacing: 6) {
                    ForEach(images, id: \.attachmentId) { img in
                        AttachmentImage(ref: img, client: client)
                            .frame(width: 88, height: 88)
                            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    }
                }
            }
            if !text.isEmpty {
                Text(text)
                    .font(.body)
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .opacity(pending ? 0.7 : 1)
            }
            if pending {
                Text(L.chatPending).font(.caption2).foregroundStyle(Theme.textMuted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.leading, 40)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(L.chatA11yUserMessage(text))
        .contextMenu {
            Button { UIPasteboard.general.string = text } label: { Label(L.chatCopyText, systemImage: "doc.on.doc") }
        }
    }
}

/// The turn's answer: Markdown with copy and "send again" where the web UI offers them.
struct AnswerView: View {
    let text: String
    let chat: ChatModel
    let turn: Turn
    let selection: ChatSelection

    var body: some View {
        MarkdownView(text: text)
            .accessibilityLabel(L.chatA11yAssistant(text))
            .contextMenu {
                Button { UIPasteboard.general.string = text } label: { Label(L.commonCopy, systemImage: "doc.on.doc") }
            }
    }
}

/// Goal, reminder and Team lines, MCP switches, "Gestoppt" and "Unterbrochen".
struct NoticeView: View {
    let item: NoticeItem

    var body: some View {
        switch item.code {
        case "mcp-activation", "mcp-deactivation":
            sourceLine(L.chatMcpLabel, item.code == "mcp-activation" ? L.chatMcpOn(item.server ?? "") : L.chatMcpOff(item.server ?? ""))
        case "job":
            sourceLine(L.chatJobLabel, item.text ?? "")
        case "goal":
            sourceLine(L.chatGoal, L.chatGoalRounds(item.round.map(String.init) ?? "?", item.max ?? "∞"))
        case "schedule":
            sourceLine(L.chatAutomations, item.prompt ?? "")
        case "team-message":
            sourceLine(L.chatTeamMessageFrom(item.sender ?? ""), item.text ?? "")
        case "interrupted":
            Text(L.chatInterrupted).font(.footnote).foregroundStyle(Theme.warning)
        default:
            Text(L.chatStopped).font(.footnote).foregroundStyle(Theme.textMuted)
        }
    }

    private func sourceLine(_ label: String, _ detail: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(label).font(.caption.weight(.semibold)).foregroundStyle(Theme.textMuted)
            if !detail.isEmpty { Text(detail).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(2) }
        }
        .accessibilityElement(children: .combine)
    }
}

/// A failed turn: what happened in plain words, the provider's own message, and what helps.
struct FailureNotice: View {
    let item: NoticeItem
    let turn: Turn
    let chat: ChatModel
    let selection: ChatSelection

    private var code: String { item.failure?["code"]?.string ?? "" }
    private var known: (String, String)? {
        guard let title = L.lookup("chat.failure.\(code).title"), let text = L.lookup("chat.failure.\(code).text") else { return nil }
        return (title, text)
    }

    var body: some View {
        let message = item.failure?["message"]?.string ?? ""
        let status = item.failure?["status"]?.int
        let idle = (chat.status == "idle" || chat.status == "error") && chat.pending == nil
        let last = chat.transcript.turns.last?.turn == turn.turn
        let firstUser = turn.items.compactMap { if case .user(let u) = $0 { return u } else { return nil } }.first
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger)
                VStack(alignment: .leading, spacing: 4) {
                    Text(known?.0 ?? L.chatTurnFailed).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    if let known { Text(known.1).font(.footnote).foregroundStyle(Theme.text) }
                    if !message.isEmpty || !code.isEmpty {
                        Text((message.isEmpty ? "" : message + " ") + (code.isEmpty ? "" : "\(code)\(status.map { " · \($0)" } ?? "")"))
                            .font(.caption).foregroundStyle(Theme.textMuted)
                    }
                    if idle, last, let u = firstUser {
                        Button {
                            let r = selection.resolve(info: chat.device.info, transcript: chat.transcript)
                            Task { await chat.send(text: u.text, images: u.images, options: .init(model: r.model, effort: r.effort, sandbox: r.sandbox, steer: false)) }
                        } label: { Label(L.chatResend, systemImage: "arrow.clockwise") }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                    }
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.danger.opacity(0.08), in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
        .accessibilityElement(children: .combine)
    }
}

/// dsh `message.maxTokens.hint` with a button that sends "weiter" in the prompt's language.
struct MaxTokensNotice: View {
    let turn: Turn
    let chat: ChatModel
    let selection: ChatSelection

    var body: some View {
        let idle = (chat.status == "idle" || chat.status == "error") && chat.pending == nil
        let last = chat.transcript.turns.last?.turn == turn.turn
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(Theme.warning)
                VStack(alignment: .leading, spacing: 4) {
                    Text(L.chatMaxTokensTitle).font(.subheadline.weight(.semibold))
                    Text(L.chatMaxTokensHint).font(.footnote)
                    if idle, last {
                        Button(L.chatContinue) {
                            let r = selection.resolve(info: chat.device.info, transcript: chat.transcript)
                            let word = continueWord(turn.items.compactMap { if case .user(let u) = $0 { return u.text } else { return nil } }.first ?? "")
                            Task { await chat.send(text: word, images: [], options: .init(model: r.model, effort: r.effort, sandbox: r.sandbox, steer: false)) }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                    }
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.warning.opacity(0.1), in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
    }

    /// "weiter" or "continue": the language of the prompt, else the UI's.
    private func continueWord(_ text: String) -> String {
        let german = text.lowercased().split(whereSeparator: { !$0.isLetter }).filter { ["und", "der", "die", "das", "nicht", "ist", "ich", "du", "bitte", "mit", "für", "auf", "eine", "ein", "wie", "was", "warum", "noch", "mach"].contains(String($0)) }.count
            + text.filter { "äöüß".contains($0) }.count
        let english = text.lowercased().split(whereSeparator: { !$0.isLetter }).filter { ["the", "and", "is", "are", "please", "with", "for", "what", "why", "how", "this", "that", "you", "can", "make"].contains(String($0)) }.count
        if german > english { return "weiter" }
        if english > german { return "continue" }
        return L.chatContinueWord
    }
}

/// "N Einträge komprimiert (~X Tokens)"; a tap shows the summary.
struct CompactionView: View {
    let item: DividerItem
    @State private var open = false

    var body: some View {
        if item.running {
            HStack(spacing: 8) {
                Image(systemName: "rectangle.compress.vertical").font(.footnote).foregroundStyle(Theme.textMuted)
                ShimmerText(text: L.chatCompactionRunning, active: true)
            }
        } else {
            let expandable = item.summary != nil
            let summary: String = {
                if item.error != nil { return "" }
                if let n = item.items, let t = item.tokens { return L.chatCompactionDone(n, Format.compact(t)) }
                return expandable ? L.chatCompactionExpand : L.chatCompactionUnavailable
            }()
            VStack(alignment: .leading, spacing: 6) {
                Button { withAnimation(.snappy) { open.toggle() } } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "rectangle.compress.vertical").font(.footnote).foregroundStyle(Theme.textMuted)
                        Text(item.error != nil ? L.chatCompactionFailed : L.chatCompaction).font(.footnote.weight(.medium)).foregroundStyle(Theme.textMuted)
                        if !summary.isEmpty {
                            Text("·").foregroundStyle(Theme.textMuted)
                            Text(summary).font(.footnote).foregroundStyle(Theme.textMuted).lineLimit(1)
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
                if open, let s = item.summary {
                    MarkdownView(text: s, muted: true).padding(.leading, 24)
                }
            }
        }
    }
}
