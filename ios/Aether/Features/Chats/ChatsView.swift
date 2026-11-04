import SwiftUI

/// Chats of one device: pinned on top, projects with their chats, chats without a project; newest activity first.
struct ChatsView: View {
    @Environment(AppModel.self) private var model
    let connection: DeviceConnection

    @State private var search = ""
    @State private var showArchived = false
    @State private var newChat = false
    @State private var renaming: SessionRow?
    @State private var deleting: SessionRow?

    private struct Project: Identifiable {
        var cwd: String
        var name: String
        var sessions: [SessionRow]
        var latest: Double
        var id: String { cwd }
    }

    private var listed: [SessionRow] {
        let q = search.trimmingCharacters(in: .whitespaces).lowercased()
        return connection.sessions.filter { s in
            (showArchived || !s.archived) && (q.isEmpty || s.title.lowercased().contains(q) || Format.folderName(s.cwd).lowercased().contains(q))
        }
    }

    private var pinned: [SessionRow] { listed.filter(\.pinned).sorted { $0.when > $1.when } }
    private var loose: [SessionRow] { listed.filter { $0.scratch && !$0.pinned }.sorted { $0.when > $1.when } }

    private var projects: [Project] {
        var groups: [String: Project] = [:]
        var order: [String] = []
        for w in connection.workspaces where search.isEmpty {
            groups[w.path] = Project(cwd: w.path, name: w.name, sessions: [], latest: w.lastUsed ?? 0)
            order.append(w.path)
        }
        for s in listed where !s.scratch && !s.pinned {
            if groups[s.cwd] == nil {
                groups[s.cwd] = Project(cwd: s.cwd, name: Format.folderName(s.cwd), sessions: [], latest: 0)
                order.append(s.cwd)
            }
            groups[s.cwd]?.sessions.append(s)
            let latest = groups[s.cwd]?.latest ?? 0
            groups[s.cwd]?.latest = max(latest, s.when)
        }
        return order.compactMap { groups[$0] }
            .map { var p = $0; p.sessions.sort { $0.when > $1.when }; return p }
            .filter { !$0.sessions.isEmpty || search.isEmpty }
            .sorted { $0.latest > $1.latest }
    }

    var body: some View {
        @Bindable var model = model
        List(selection: $model.selectedChat) {
            if !pinned.isEmpty {
                Section(L.chatsPinned) {
                    ForEach(pinned) { s in row(s, project: s.scratch ? nil : Format.folderName(s.cwd)) }
                }
            }
            ForEach(projects) { p in
                Section {
                    if p.sessions.isEmpty {
                        Button { newChat = true } label: {
                            Label(L.chatsNew, systemImage: "square.and.pencil").font(.subheadline).foregroundStyle(Theme.textMuted)
                        }
                    }
                    ForEach(p.sessions) { s in row(s, project: nil) }
                } header: {
                    Label(p.name, systemImage: "folder")
                }
            }
            if !loose.isEmpty {
                Section(L.chatsNoProject) {
                    ForEach(loose) { s in row(s, project: nil) }
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(Theme.bg)
        .overlay { stateOverlay }
        .safeAreaInset(edge: .top, spacing: 0) {
            // The list is cached: when the device stops answering (or revoked this phone), say so above it.
            if !connection.sessions.isEmpty, let problem = connectionProblem {
                Banner(text: problem, tone: .error)
                    .padding(.horizontal)
                    .padding(.vertical, 6)
            }
        }
        .searchable(text: $search, prompt: L.chatsSearch)
        .navigationTitle(connection.device.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarTitleMenu {
            ForEach(model.devices) { d in
                Button {
                    model.selectedDeviceID = d.id
                    model.selectedChat = nil
                } label: {
                    Label(d.name, systemImage: d.id == connection.id ? "checkmark" : "laptopcomputer")
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { newChat = true } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel(L.chatsNew)
                    .disabled(!connection.status.isReady)
            }
            ToolbarItem(placement: .secondaryAction) {
                Toggle(isOn: $showArchived) { Label(L.chatsShowArchived, systemImage: "archivebox") }
            }
            ToolbarItem(placement: .secondaryAction) {
                Button { connection.connect(force: true) } label: { Label(L.devicesReconnect, systemImage: "arrow.clockwise") }
            }
        }
        .task(id: connection.status) {
            // Every visit refreshes; a device that just came up loads its list.
            if connection.status.isReady { await connection.refreshSessions() }
        }
        .refreshable {
            if connection.status.isReady { await connection.refreshSessions() } else { connection.connect(force: true) }
        }
        .sheet(isPresented: $newChat) { NewChatSheet(connection: connection) }
        .alert(L.chatsRenameTitle, isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            RenameFields(initial: renaming?.title ?? "") { title in
                if let r = renaming { Task { await connection.patchSession(r.id, patch: ["title": .string(title.trimmingCharacters(in: .whitespaces))]) } }
                renaming = nil
            } cancel: { renaming = nil }
        }
        .confirmationDialog(L.chatsDelete, isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }), titleVisibility: .visible) {
            Button(L.chatsDelete, role: .destructive) {
                if let d = deleting {
                    Task {
                        try? await connection.deleteSession(d.id)
                        if model.selectedChat?.sessionID == d.id { model.selectedChat = nil }
                    }
                }
                deleting = nil
            }
            Button(L.commonCancel, role: .cancel) { deleting = nil }
        } message: {
            Text(L.chatsDeleteConfirm)
        }
    }

    private var connectionProblem: String? {
        if case .error(let message) = connection.status { return message }
        return connection.sessionsError
    }

    @ViewBuilder
    private var stateOverlay: some View {
        if connection.loadingSessions, connection.sessions.isEmpty {
            ProgressView(L.commonLoading)
        } else if case .connecting = connection.status, connection.sessions.isEmpty {
            ProgressView(L.devicesStatusConnecting)
        } else if case .error(let message) = connection.status, connection.sessions.isEmpty {
            ContentUnavailableView {
                Label(L.devicesStatusError, systemImage: "wifi.exclamationmark")
            } description: {
                Text(message)
            } actions: {
                Button(L.commonRetry) { connection.connect(force: true) }.buttonStyle(.borderedProminent)
            }
        } else if let error = connection.sessionsError, connection.sessions.isEmpty {
            ContentUnavailableView {
                Label(L.chatsLoadFailed, systemImage: "exclamationmark.triangle")
            } description: {
                Text(error)
            } actions: {
                Button(L.commonRetry) { Task { await connection.refreshSessions() } }.buttonStyle(.borderedProminent)
            }
        } else if connection.status.isReady, listed.isEmpty, projects.isEmpty {
            if search.isEmpty {
                ContentUnavailableView {
                    Label(L.chatsEmptyTitle, systemImage: "bubble.left.and.bubble.right")
                } description: {
                    Text(L.chatsEmptyText)
                } actions: {
                    Button(L.chatsNew) { newChat = true }.buttonStyle(.borderedProminent)
                }
            } else {
                ContentUnavailableView.search(text: search)
            }
        }
    }

    private func row(_ s: SessionRow, project: String?) -> some View {
        ChatRow(session: s, project: project)
            .tag(AppModel.ChatRef(deviceID: connection.id, sessionID: s.id))
            .swipeActions(edge: .leading) {
                Button { Task { await connection.patchSession(s.id, patch: ["pinned": .bool(!s.pinned)]) } } label: {
                    Label(s.pinned ? L.chatsUnpin : L.chatsPin, systemImage: s.pinned ? "pin.slash" : "pin")
                }.tint(Theme.accent)
            }
            .swipeActions(edge: .trailing) {
                Button(role: .destructive) { deleting = s } label: { Label(L.chatsDelete, systemImage: "trash") }
                Button { Task { await connection.patchSession(s.id, patch: ["archived": .bool(!s.archived)]) } } label: {
                    Label(s.archived ? L.chatsUnarchive : L.chatsArchive, systemImage: s.archived ? "tray.and.arrow.up" : "archivebox")
                }.tint(Theme.textMuted)
            }
            .contextMenu {
                Button { renaming = s } label: { Label(L.commonRename, systemImage: "pencil") }
                Button { Task { await connection.patchSession(s.id, patch: ["pinned": .bool(!s.pinned)]) } } label: {
                    Label(s.pinned ? L.chatsUnpin : L.chatsPin, systemImage: "pin")
                }
                Button { Task { await connection.patchSession(s.id, patch: s.unread ? ["seen": .bool(true)] : ["unread": .bool(true)]) } } label: {
                    Label(s.unread ? L.chatsMarkRead : L.chatsMarkUnread, systemImage: s.unread ? "envelope.open" : "envelope.badge")
                }
                Divider()
                Button { Task { await connection.patchSession(s.id, patch: ["archived": .bool(!s.archived)]) } } label: {
                    Label(s.archived ? L.chatsUnarchive : L.chatsArchive, systemImage: "archivebox")
                }
                Button(role: .destructive) { deleting = s } label: { Label(L.chatsDelete, systemImage: "trash") }
            }
    }
}

struct ChatRow: View {
    let session: SessionRow
    var project: String?

    private var mark: (color: Color, label: String, spinning: Bool)? {
        if session.status == "waiting-approval" { return (Theme.warning, L.chatsStatusWaiting, false) }
        if session.status == "running" { return (Theme.accent, L.chatsStatusRunning, true) }
        if session.status == "error" { return (Theme.danger, L.chatsStatusError, false) }
        if session.unread { return (Theme.accent, L.chatsStatusUnread, false) }
        return nil
    }

    var body: some View {
        HStack(spacing: 10) {
            ZStack {
                if let mark {
                    if mark.spinning {
                        ProgressView().controlSize(.mini).accessibilityLabel(mark.label)
                    } else {
                        StatusDot(color: mark.color, label: mark.label)
                    }
                } else {
                    Image(systemName: "bubble.left").font(.footnote).foregroundStyle(Theme.textMuted).accessibilityHidden(true)
                }
            }
            .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text(session.title.isEmpty ? L.chatsUntitled : session.title)
                    .font(.body.weight(session.unread ? .semibold : .regular))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                HStack(spacing: 6) {
                    if let project { Text(project).font(.caption).foregroundStyle(Theme.textMuted) }
                    if session.archived { Chip(text: L.chatsArchivedTag) }
                    if let mark, mark.spinning || session.status == "waiting-approval" {
                        Text(mark.label).font(.caption).foregroundStyle(mark.color)
                    }
                }
            }
            Spacer(minLength: 4)
            Text(Format.age(ms: session.when > 0 ? session.when : Date().timeIntervalSince1970 * 1000))
                .font(.caption)
                .foregroundStyle(Theme.textMuted)
                .monospacedDigit()
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}
