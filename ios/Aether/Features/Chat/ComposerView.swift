import PhotosUI
import SwiftUI

/// The composer: multiline field, "+" for photos, model / effort / access, send or stop.
/// While a turn runs a message steers it by default (long press on send for the other one).
struct ComposerView: View {
    @Environment(AppModel.self) private var app
    let chat: ChatModel
    @Binding var selection: ChatSelection
    @State private var text = ""
    @State private var photos: [PhotosPickerItem] = []
    @State private var attachments: [Attachment] = []
    @State private var uploading = false
    @FocusState private var focused: Bool

    struct Attachment: Identifiable {
        var id = UUID()
        var image: UIImage
        var ref: ImageAttachmentRef
    }

    private var info: ServerInfo? { chat.device.info }
    private var resolved: ChatSelection.Resolved { selection.resolve(info: info, transcript: chat.transcript) }
    private var running: Bool { chat.isRunning }
    private var canSend: Bool {
        (info?.apiKeyConfigured ?? true) && !resolved.missing && !uploading && (!text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty)
    }
    private var showStop: Bool { running && !canSend }

    var body: some View {
        VStack(spacing: 8) {
            if !attachments.isEmpty || uploading {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(attachments) { a in
                            Image(uiImage: a.image)
                                .resizable().scaledToFill()
                                .frame(width: 56, height: 56)
                                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                                .overlay(alignment: .topTrailing) {
                                    Button { attachments.removeAll { $0.id == a.id } } label: {
                                        Image(systemName: "xmark.circle.fill").font(.footnote).foregroundStyle(.white, .black.opacity(0.6))
                                    }
                                    .accessibilityLabel(L.chatRemoveImage)
                                    .padding(2)
                                }
                        }
                        if uploading { ProgressView().frame(width: 56, height: 56) }
                    }
                }
            }
            TextField(placeholder, text: $text, axis: .vertical)
                .lineLimit(1...6)
                .focused($focused)
                .textFieldStyle(.plain)
                .font(.body)
                .padding(.horizontal, 4)
                .accessibilityLabel(L.chatPlaceholder)
            HStack(spacing: 8) {
                PhotosPicker(selection: $photos, maxSelectionCount: 4, matching: .images) {
                    Image(systemName: "plus").font(.body.weight(.medium)).frame(width: 32, height: 32)
                        .background(Theme.surfaceRaised, in: Circle())
                }
                .accessibilityLabel(L.chatAttach)
                .disabled(!(resolved.modelInfo?.acceptsImages ?? true))
                modelMenu
                accessMenu
                Spacer(minLength: 0)
                sendButton
            }
        }
        .padding(10)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous).stroke(Theme.border))
        .onChange(of: photos) { _, items in
            guard !items.isEmpty else { return }
            photos = []
            Task { await addPhotos(items) }
        }
    }

    private var placeholder: String {
        if let info, !info.apiKeyConfigured { return L.chatNoKey }
        if running { return app.preferences.followup == .steer ? L.chatPlaceholderRunningSteer : L.chatPlaceholderRunningQueue }
        return chat.transcript.turns.isEmpty ? L.chatPlaceholderHero : L.chatPlaceholder
    }

    private var modelMenu: some View {
        let r = resolved
        let models = info?.models ?? []
        return Menu {
            Section(L.chatModel) {
                ForEach(models) { m in
                    Button {
                        selection.model = m.id
                        selection.effort = nil
                    } label: {
                        if m.id == r.model { Label(m.name, systemImage: "checkmark") } else { Text(m.name) }
                    }
                    .disabled(m.available == false)
                }
            }
            if let efforts = r.modelInfo?.reasoningEfforts, !efforts.isEmpty {
                Section(L.chatEffort) {
                    ForEach(efforts, id: \.self) { e in
                        Button { selection.effort = e } label: {
                            if e == r.effort { Label(effortName(e), systemImage: "checkmark") } else { Text(effortName(e)) }
                        }
                    }
                }
            }
        } label: {
            HStack(spacing: 4) {
                Text(r.modelInfo?.name ?? r.model).lineLimit(1)
                if let e = r.effort { Text("· \(effortName(e))").foregroundStyle(Theme.textMuted) }
                Image(systemName: "chevron.down").font(.caption2)
            }
            .font(.caption.weight(.medium))
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(Theme.surfaceRaised, in: Capsule())
        }
        .accessibilityLabel(L.chatModel)
        .disabled(models.isEmpty)
    }

    private var accessMenu: some View {
        let r = resolved
        let modes: [(String, String, String)] = [("read-only", L.chatAccessReadOnly, L.chatAccessReadOnlyHint), ("workspace-write", L.chatAccessWorkspace, L.chatAccessWorkspaceHint),
                                                  ("danger-full-access", L.chatAccessFull, L.chatAccessFullHint)]
        return Menu {
            ForEach(modes, id: \.0) { mode, name, hint in
                Button { selection.sandbox = mode } label: {
                    if mode == r.sandbox { Label("\(name) · \(hint)", systemImage: "checkmark") } else { Text("\(name) · \(hint)") }
                }
            }
        } label: {
            HStack(spacing: 4) {
                Image(systemName: r.sandbox == "danger-full-access" ? "exclamationmark.triangle" : "shield").font(.caption2)
                Text(modes.first { $0.0 == r.sandbox }?.1 ?? r.sandbox)
            }
            .font(.caption.weight(.medium))
            .foregroundStyle(r.sandbox == "danger-full-access" ? Theme.warning : Theme.text)
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(Theme.surfaceRaised, in: Capsule())
        }
        .accessibilityLabel(L.chatAccess)
    }

    private var sendButton: some View {
        let defaultSteer = app.preferences.followup == .steer
        return Button {
            if showStop { Task { await chat.cancel() } } else { submit(steer: defaultSteer) }
        } label: {
            Image(systemName: showStop ? "stop.fill" : "arrow.up")
                .font(.body.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: 34, height: 34)
                .background(showStop ? Theme.danger : (canSend ? Theme.accent : Theme.textMuted.opacity(0.5)), in: Circle())
        }
        .disabled(!showStop && !canSend)
        .accessibilityLabel(showStop ? L.chatStop : L.chatSend)
        .contextMenu {
            if running, canSend {
                Button { submit(steer: !defaultSteer) } label: {
                    Label(defaultSteer ? L.chatQueueLater : L.chatSteerNow, systemImage: defaultSteer ? "text.append" : "bolt")
                }
            }
        }
    }

    private func effortName(_ e: String) -> String { L.lookup("chat.effort.\(e)") ?? e }

    private func submit(steer: Bool) {
        guard canSend else { return }
        let typed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let refs = attachments.map(\.ref)
        text = ""
        attachments = []
        Haptics.send()
        let r = resolved
        Task {
            let ok = await chat.send(text: typed, images: refs, options: .init(model: r.model, effort: r.effort, sandbox: r.sandbox, steer: steer))
            if !ok { text = typed }
        }
    }

    private func addPhotos(_ items: [PhotosPickerItem]) async {
        uploading = true
        defer { uploading = false }
        for item in items {
            guard let data = try? await item.loadTransferable(type: Data.self), let image = UIImage(data: data) else { continue }
            // Phones take large photos; the model does not need more than ~2k pixels.
            let scaled = image.scaledDown(maxDimension: 2048)
            guard let jpeg = scaled.jpegData(compressionQuality: 0.85) else { continue }
            do {
                let ref = try await chat.upload(data: jpeg, mediaType: "image/jpeg")
                attachments.append(Attachment(image: scaled, ref: ref))
            } catch {
                chat.show(L.chatUploadFailed(ApiError.wrap(error).message))
            }
        }
    }
}

extension UIImage {
    func scaledDown(maxDimension: CGFloat) -> UIImage {
        let longest = max(size.width, size.height)
        guard longest > maxDimension else { return self }
        let scale = maxDimension / longest
        let target = CGSize(width: size.width * scale, height: size.height * scale)
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        return UIGraphicsImageRenderer(size: target, format: format).image { _ in draw(in: CGRect(origin: .zero, size: target)) }
    }
}
