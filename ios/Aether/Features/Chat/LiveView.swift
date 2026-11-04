import SwiftUI

/// Live view of browser and computer work: a small floating picture while it runs, a tap enlarges it.
struct LiveFloat: View {
    let chat: ChatModel
    var onEnlarge: () -> Void
    @State private var tick = 0

    private static let lingerMs: Double = 8000

    private var visible: LiveMessage? {
        guard let live = chat.live, !live.ended else { return nil }
        if let hidden = chat.liveHidden, hidden >= live.time - 1 { return nil }
        if !live.active, Date().timeIntervalSince1970 * 1000 - live.time > Self.lingerMs { return nil }
        return live
    }

    var body: some View {
        if let live = visible {
            VStack(alignment: .leading, spacing: 0) {
                LivePicture(live: live, client: chat.client)
                    .frame(width: 180, height: 120)
                    .clipped()
                HStack(spacing: 6) {
                    Circle().fill(live.active ? Theme.online : Theme.textMuted).frame(width: 6, height: 6)
                    Text(liveCaption(live)).font(.caption2).foregroundStyle(Theme.text).lineLimit(1)
                    Spacer(minLength: 0)
                    Button { chat.hideLive() } label: { Image(systemName: "xmark").font(.caption2.weight(.bold)) }
                        .accessibilityLabel(L.chatLiveHide)
                }
                .padding(6)
            }
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous).stroke(Theme.border))
            .shadow(color: .black.opacity(0.15), radius: 10, y: 4)
            .onTapGesture(perform: onEnlarge)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(L.chatLive)
            .accessibilityHint(L.chatLiveEnlarge)
            .task(id: live.time) {
                // Re-check once the last step has lingered long enough.
                if !live.active {
                    try? await Task.sleep(for: .milliseconds(Int(Self.lingerMs) + 50))
                    tick += 1
                }
            }
            .id(tick)
        }
    }
}

func liveCaption(_ live: LiveMessage) -> String {
    let title = live.tool == "computer" ? L.codexComputer : live.tool == "browser" ? L.codexBrowser : (live.meta?["place"]?.string ?? live.args?["app"]?.string ?? L.codexApp)
    let step = live.activity ?? (live.tool == "computer" ? CodexLabels.computer(live.args ?? .object([:])) : CodexLabels.background(tool: live.tool, args: live.args ?? .object([:]), target: live.meta?["target"]?.string))
    return step.isEmpty ? title : "\(title) · \(step)"
}

struct LivePicture: View {
    let live: LiveMessage
    let client: any ServerAPI

    var body: some View {
        ZStack {
            if let b64 = live.image, let data = Data(base64Encoded: b64), let image = UIImage(data: data) {
                Image(uiImage: image).resizable().scaledToFit()
            } else if let att = live.attachment {
                AttachmentImage(ref: att, client: client)
            } else {
                Text(L.chatLiveWaiting).font(.caption).foregroundStyle(Theme.textMuted).padding()
            }
        }
        .overlay {
            if let c = live.cursor {
                GeometryReader { g in
                    Image(systemName: "cursorarrow").font(.caption).foregroundStyle(.white).shadow(radius: 1)
                        .position(x: g.size.width * c.x, y: g.size.height * c.y)
                }
                .allowsHitTesting(false)
            }
        }
        .background(Color.black.opacity(0.05))
        .accessibilityLabel(L.chatLive)
    }
}

/// The enlarged live view with the step and a Stop button while the agent runs.
struct LiveSheet: View {
    @Environment(\.dismiss) private var dismiss
    let chat: ChatModel

    var body: some View {
        NavigationStack {
            VStack(spacing: 12) {
                if let live = chat.live {
                    LivePicture(live: live, client: chat.client)
                        .clipShape(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
                    HStack {
                        Circle().fill(live.active ? Theme.online : live.ended ? Theme.textMuted : Theme.warning).frame(width: 8, height: 8)
                        Text(live.active ? L.chatLiveActive : live.ended ? L.chatLiveEnded : L.chatLiveIdle).font(.footnote).foregroundStyle(Theme.textMuted)
                        Text("·").foregroundStyle(Theme.textMuted)
                        Text(liveCaption(live)).font(.footnote).foregroundStyle(Theme.text).lineLimit(2)
                        Spacer()
                    }
                } else {
                    ContentUnavailableView(L.chatLiveWaiting, systemImage: "rectangle.on.rectangle")
                }
                Spacer()
                if chat.isRunning {
                    Button { Task { await chat.cancel() } } label: { Label(L.chatStop, systemImage: "stop.fill") }
                        .buttonStyle(.bordered).tint(Theme.danger)
                }
            }
            .padding()
            .navigationTitle(L.chatLive)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button(L.commonDone) { dismiss() } } }
        }
    }
}
