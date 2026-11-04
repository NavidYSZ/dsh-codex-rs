import SwiftUI

/// Neuer Chat: pick the project (recent workspaces, repositories, a folder browser, or none) and the mode.
struct NewChatSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let connection: DeviceConnection

    @State private var cwd: String?
    @State private var preset: String = "standard"
    @State private var suggestions: [FsSuggestions.Repo] = []
    @State private var creating = false
    @State private var error: String?
    @State private var browse = false

    private var recent: [Workspace] { connection.workspaces.sorted { ($0.lastUsed ?? 0) > ($1.lastUsed ?? 0) } }
    private var presets: [ServerInfo.Preset] {
        connection.info?.presets ?? [.init(id: "standard", name: "Standard"), .init(id: "minimal", name: "Minimal")]
    }

    var body: some View {
        NavigationStack {
            List {
                Section(L.newchatProject) {
                    choice(path: nil, name: L.newchatNoProject, hint: L.newchatNoProjectHint, icon: "lock.doc")
                    if let picked = cwd, !recent.contains(where: { $0.path == picked }), !suggestions.contains(where: { $0.path == picked }) {
                        choice(path: picked, name: Format.folderName(picked), hint: picked, icon: "folder")
                    }
                    Button { browse = true } label: { Label(L.newchatBrowse, systemImage: "folder.badge.plus") }
                }
                if !recent.isEmpty {
                    Section(L.newchatRecent) {
                        ForEach(recent) { w in choice(path: w.path, name: w.name, hint: w.path, icon: "folder") }
                    }
                }
                let more = suggestions.filter { s in !recent.contains { $0.path == s.path } }
                if !more.isEmpty {
                    Section(L.newchatSuggested) {
                        ForEach(more) { r in choice(path: r.path, name: r.name, hint: r.path, icon: "shippingbox") }
                    }
                }
                Section(L.newchatMode) {
                    Picker(L.newchatMode, selection: $preset) {
                        ForEach(presets) { p in Text(p.id == "minimal" ? L.newchatModeMinimal : p.id == "standard" ? L.newchatModeStandard : p.name).tag(p.id) }
                    }
                    .pickerStyle(.segmented)
                    Text(preset == "minimal" ? L.newchatModeMinimalHint : L.newchatModeStandardHint).font(.footnote).foregroundStyle(Theme.textMuted)
                }
                if let error { Section { Text(error).font(.footnote).foregroundStyle(Theme.danger) } }
            }
            .navigationTitle(L.newchatTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(L.commonCancel) { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button { start() } label: {
                        if creating { ProgressView() } else { Text(L.newchatStart).bold() }
                    }
                    .disabled(creating)
                }
            }
            .navigationDestination(isPresented: $browse) {
                FolderBrowser(connection: connection, start: cwd ?? recent.first?.path ?? "") { path in
                    cwd = path
                    browse = false
                }
            }
            .task {
                if cwd == nil {
                    let last = model.preferences.lastProject[connection.id]
                    if let last, last.isEmpty { cwd = nil }
                    else if let last, recent.contains(where: { $0.path == last }) { cwd = last }
                    else { cwd = recent.first?.path }
                }
                preset = connection.info?.settings.defaultPreset ?? "standard"
                if let s = try? await connection.client?.fsSuggestions() { suggestions = s.repos }
            }
        }
    }

    private func choice(path: String?, name: String, hint: String, icon: String) -> some View {
        Button {
            cwd = path
        } label: {
            HStack(spacing: 12) {
                Image(systemName: icon).foregroundStyle(Theme.accent).frame(width: 24)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).foregroundStyle(Theme.text)
                    Text(hint).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(1).truncationMode(.middle)
                }
                Spacer()
                if cwd == path { Image(systemName: "checkmark").foregroundStyle(Theme.accent) }
            }
        }
        .accessibilityAddTraits(cwd == path ? .isSelected : [])
    }

    private func start() {
        creating = true
        error = nil
        Task {
            do {
                let id = try await connection.createSession(cwd: cwd, preset: preset)
                Haptics.light()
                model.selectedChat = AppModel.ChatRef(deviceID: connection.id, sessionID: id)
                dismiss()
            } catch {
                self.error = L.newchatFailed(ApiError.wrap(error).message)
            }
            creating = false
        }
    }
}

/// A folder browser over `/api/fs/dirs`; a path field when the server cannot list folders.
struct FolderBrowser: View {
    let connection: DeviceConnection
    let start: String
    var onPick: (String) -> Void

    @State private var listing: FsDirs?
    @State private var error: String?
    @State private var unsupported = false
    @State private var hidden = false
    @State private var manual = ""
    @State private var loading = false

    var body: some View {
        List {
            if unsupported {
                Section {
                    Text(L.newchatFolderUnavailable).font(.footnote).foregroundStyle(Theme.textMuted)
                    TextField(L.newchatFolderPath, text: $manual, prompt: Text(L.newchatFolderPathPlaceholder))
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    Button(L.newchatFolderUseThis) { onPick(manual.trimmingCharacters(in: .whitespaces)) }
                        .disabled(!manual.hasPrefix("/"))
                }
            } else if let listing {
                Section {
                    HStack {
                        Image(systemName: "folder.fill").foregroundStyle(Theme.accent)
                        Text(listing.path).font(.footnote.monospaced()).lineLimit(2).truncationMode(.middle)
                        Spacer()
                        if listing.git == true { Chip(text: L.newchatFolderGit) }
                    }
                    Button(L.newchatFolderUseThis) { onPick(listing.path) }.bold()
                    if let parent = listing.parent, !parent.isEmpty {
                        Button { load(parent) } label: { Label(L.newchatFolderParent, systemImage: "arrow.up") }
                    }
                    if let home = listing.home, home != listing.path {
                        Button { load(home) } label: { Label(L.newchatFolderHome, systemImage: "house") }
                    }
                }
                Section {
                    if listing.entries.isEmpty {
                        Text(L.newchatFolderEmpty).font(.footnote).foregroundStyle(Theme.textMuted)
                    }
                    ForEach(listing.entries) { e in
                        Button { load(e.path) } label: {
                            HStack {
                                Image(systemName: "folder").foregroundStyle(Theme.textMuted)
                                Text(e.name).foregroundStyle(e.hidden == true ? Theme.textMuted : Theme.text)
                                Spacer()
                                if e.git == true { Image(systemName: "shippingbox").font(.caption).foregroundStyle(Theme.textMuted) }
                                Image(systemName: "chevron.right").font(.caption).foregroundStyle(Theme.textMuted)
                            }
                        }
                    }
                } footer: {
                    if listing.truncated == true { Text(L.newchatFolderEmpty) }
                }
            }
            if let error { Section { Text(error).font(.footnote).foregroundStyle(Theme.danger) } }
        }
        .overlay { if loading, listing == nil { ProgressView() } }
        .navigationTitle(L.newchatBrowse)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .secondaryAction) {
                Toggle(isOn: $hidden) { Label(L.newchatFolderHidden, systemImage: "eye") }
            }
        }
        .onChange(of: hidden) { _, _ in load(listing?.path ?? start) }
        .task { load(start) }
    }

    private func load(_ path: String) {
        guard let client = connection.client else { return }
        loading = true
        error = nil
        Task {
            do {
                listing = try await client.fsDirs(path: path, hidden: hidden)
                unsupported = false
            } catch {
                let e = ApiError.wrap(error)
                switch e {
                case .http(404, _, "unsupported"), .http(501, _, _): unsupported = true
                case .http(403, _, _): self.error = L.newchatFolderDenied
                case .http(404, _, _): self.error = L.newchatFolderNotFound
                default: self.error = e.message
                }
            }
            loading = false
        }
    }
}
