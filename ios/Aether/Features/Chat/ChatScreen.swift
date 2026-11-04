import SwiftUI

/// Opens one chat of a device: creates its model once the device is connected, then shows `ChatView`.
struct ChatScreen: View {
    @Environment(AppModel.self) private var model
    let ref: AppModel.ChatRef
    let connection: DeviceConnection
    @State private var chat: ChatModel?

    var body: some View {
        Group {
            if let chat {
                ChatView(chat: chat)
            } else if case .error(let message) = connection.status {
                ContentUnavailableView {
                    Label(L.chatsNotReady, systemImage: "wifi.exclamationmark")
                } description: {
                    Text(message)
                } actions: {
                    Button(L.commonRetry) { connection.connect(force: true) }.buttonStyle(.borderedProminent)
                }
            } else {
                ProgressView(L.chatStatusConnecting)
            }
        }
        .task(id: connection.status) {
            guard let client = connection.client else { return }
            // A reconnect replaces the device's client; the chat follows it.
            let m: ChatModel
            if let chat, chat.client === client {
                m = chat
            } else {
                chat?.stop()
                m = ChatModel(sessionID: ref.sessionID, client: client, device: connection)
                chat = m
            }
            await m.resume()
        }
        .onDisappear { chat?.stop() }
    }
}

/// The chat: transcript, approval banner, plan, composer; the status chip under the title.
struct ChatView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    let chat: ChatModel
    @State private var selection = ChatSelection()
    @State private var liveEnlarged = false

    private var title: String { chat.title.isEmpty ? L.chatsUntitled : chat.title }

    var body: some View {
        TranscriptList(chat: chat, selection: selection)
            .background(Theme.bg)
            .safeAreaInset(edge: .bottom, spacing: 0) {
                VStack(spacing: 8) {
                    if let toast = chat.toast {
                        Banner(text: toast, tone: .error)
                    }
                    if chat.connection == .retry {
                        Banner(text: L.chatStatusReconnecting, tone: .muted)
                    }
                    if let info = chat.device.info, !info.apiKeyConfigured {
                        Banner(text: L.chatNoKey, tone: .info)
                    }
                    if let approval = chat.currentApproval {
                        ApprovalBanner(approval: approval) { decision in
                            Task { await chat.decide(callId: approval.callId, decision: decision) }
                        }
                    }
                    PlanBar(transcript: chat.transcript, running: chat.isRunning)
                    ComposerView(chat: chat, selection: $selection)
                }
                .padding(.horizontal, 12)
                .padding(.top, 8)
                .padding(.bottom, 8)
                .background(.bar)
            }
            .overlay(alignment: .bottomTrailing) {
                LiveFloat(chat: chat) { liveEnlarged = true }
                    .padding(.trailing, 12)
                    .padding(.bottom, 8)
            }
            .sheet(isPresented: $liveEnlarged) { LiveSheet(chat: chat) }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 1) {
                        Text(title).font(.headline).lineLimit(1)
                        StatusChip(chat: chat)
                    }
                }
                ToolbarItem(placement: .primaryAction) {
                    Menu {
                        Button { Task { await chat.reconnect() } } label: { Label(L.devicesReconnect, systemImage: "arrow.clockwise") }
                        if let stats = chat.stats, let pct = stats.contextPercent {
                            Text(L.chatUsageContext(Format.percent(pct)))
                        }
                        if let cwd = Optional(chat.cwd), !cwd.isEmpty {
                            Text(cwd)
                        }
                    } label: { Image(systemName: "ellipsis.circle") }
                        .accessibilityLabel(L.commonMore)
                }
            }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { Task { await chat.reconnect() } }
            }
            .onChange(of: chat.transcript.title) { _, _ in chat.device.refreshSoon() }
    }
}

/// running / waiting / idle / error and the connection, as one small line under the title.
struct StatusChip: View {
    let chat: ChatModel

    private var text: (String, Color) {
        if !chat.loaded { return (L.chatStatusConnecting, Theme.textMuted) }
        if chat.loadError != nil { return (L.chatStatusError, Theme.danger) }
        switch chat.connection {
        case .connecting: return (L.chatStatusConnecting, Theme.textMuted)
        case .retry: return (L.chatStatusReconnecting, Theme.warning)
        case .offline: return (L.chatStatusOffline, Theme.danger)
        case .ok: break
        }
        switch chat.status {
        case "running": return (L.chatStatusRunning, Theme.accent)
        case "waiting-approval": return (L.chatStatusWaiting, Theme.warning)
        case "error": return (L.chatStatusError, Theme.danger)
        default: return (L.chatStatusIdle, Theme.online)
        }
    }

    var body: some View {
        let (label, color) = text
        HStack(spacing: 4) {
            if chat.status == "running", chat.connection == .ok {
                ProgressView().controlSize(.mini)
            } else {
                Circle().fill(color).frame(width: 6, height: 6)
            }
            Text(label).font(.caption2).foregroundStyle(Theme.textMuted)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(label)
    }
}

struct Banner: View {
    enum Tone { case error, muted, info }
    var text: String
    var tone: Tone

    var body: some View {
        Text(text)
            .font(.footnote)
            .foregroundStyle(tone == .error ? Theme.danger : Theme.text)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(tone == .error ? Theme.danger.opacity(0.1) : tone == .info ? Theme.accentSoft : Theme.surfaceRaised,
                        in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
            .accessibilityAddTraits(.isStaticText)
    }
}

/// Model, effort and access for the next prompt: composer pick > session > the server's defaults (ui `useSelection`).
struct ChatSelection: Hashable {
    var model: String?
    var effort: String?
    var sandbox: String?

    struct Resolved: Hashable {
        var model: String
        var modelInfo: ModelInfo?
        var effort: String?
        var sandbox: String
        var missing: Bool
    }

    func resolve(info: ServerInfo?, transcript: Transcript) -> Resolved {
        let d = info?.settings
        let modelId = model ?? transcript.model ?? d?.model ?? info?.models.first?.id ?? ""
        let modelInfo = info?.models.first { $0.id == modelId }
        var effort = self.effort ?? transcript.effort ?? d?.effort ?? modelInfo?.defaultReasoningEffort
        if let efforts = modelInfo?.reasoningEfforts, !efforts.isEmpty, let e = effort, !efforts.contains(e) { effort = modelInfo?.defaultReasoningEffort }
        let sandbox = self.sandbox ?? transcript.sandbox ?? d?.sandbox ?? "workspace-write"
        let missing = !modelId.isEmpty && !(info?.models.isEmpty ?? true) && modelInfo == nil
        return Resolved(model: modelId, modelInfo: modelInfo, effort: missing ? nil : effort, sandbox: sandbox, missing: missing)
    }
}
