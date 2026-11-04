import SwiftUI

/// Geräte: a card per device with its status, transport and counts. "+" adds one, the gear opens Settings.
struct DevicesView: View {
    @Environment(AppModel.self) private var model
    @State private var renaming: Device?
    @State private var removing: Device?
    @State private var repairing = false

    var body: some View {
        @Bindable var model = model
        List(selection: $model.selectedDeviceID) {
            ForEach(model.devices) { device in
                if let connection = model.connection(for: device.id) {
                    DeviceCard(connection: connection)
                        .tag(device.id)
                        .listRowSeparator(.hidden)
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button(role: .destructive) { removing = device } label: { Label(L.commonRemove, systemImage: "trash") }
                            Button { renaming = device } label: { Label(L.commonRename, systemImage: "pencil") }.tint(Theme.accent)
                        }
                        .contextMenu {
                            Button { connection.connect(force: true) } label: { Label(L.devicesReconnect, systemImage: "arrow.clockwise") }
                            Button { renaming = device } label: { Label(L.commonRename, systemImage: "pencil") }
                            if !device.isDemo {
                                Button { repairing = true } label: { Label(L.devicesRepair, systemImage: "qrcode") }
                            }
                            Divider()
                            Button(role: .destructive) { removing = device } label: { Label(L.devicesRemove, systemImage: "trash") }
                        }
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Theme.bg)
        .overlay {
            if model.devices.isEmpty {
                ContentUnavailableView {
                    Label(L.devicesEmptyTitle, systemImage: "laptopcomputer.and.iphone")
                } description: {
                    Text(L.devicesEmptyText)
                } actions: {
                    Button(L.devicesAdd) { model.showAddDevice = true }.buttonStyle(.borderedProminent)
                    Button(L.welcomeDemo) { model.addDemo() }
                }
            }
        }
        .navigationTitle(L.devicesTitle)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { model.showSettings = true } label: { Image(systemName: "gearshape") }
                    .accessibilityLabel(L.settingsTitle)
            }
            ToolbarItem(placement: .primaryAction) {
                Button { model.showAddDevice = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel(L.devicesAdd)
            }
        }
        .refreshable { model.connectAll(force: true) }
        .alert(L.devicesRename, isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            RenameFields(initial: renaming?.name ?? "") { name in
                if let r = renaming { model.rename(r.id, to: name) }
                renaming = nil
            } cancel: { renaming = nil }
        }
        .confirmationDialog(L.devicesRemove, isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), titleVisibility: .visible) {
            Button(L.devicesRemove, role: .destructive) {
                if let r = removing { model.remove(r.id) }
                removing = nil
            }
            Button(L.commonCancel, role: .cancel) { removing = nil }
        } message: {
            Text(L.devicesRemoveConfirm(removing?.name ?? ""))
        }
        .sheet(isPresented: $repairing) { AddDeviceView() }
    }
}

/// The text field and buttons of a rename alert.
struct RenameFields: View {
    @State private var name: String
    var save: (String) -> Void
    var cancel: () -> Void

    init(initial: String, save: @escaping (String) -> Void, cancel: @escaping () -> Void) {
        _name = State(initialValue: initial)
        self.save = save
        self.cancel = cancel
    }

    var body: some View {
        TextField(L.commonName, text: $name)
        Button(L.commonSave) { save(name) }
        Button(L.commonCancel, role: .cancel) { cancel() }
    }
}

struct DeviceCard: View {
    let connection: DeviceConnection

    private var device: Device { connection.device }

    private var icon: String {
        switch device.kind {
        case .laptop: return "laptopcomputer"
        case .server: return "server.rack"
        case .cloud: return "cloud"
        case .demo: return "play.rectangle"
        }
    }

    private var statusColor: Color {
        switch connection.status {
        case .ready: return Theme.online
        case .connecting: return Theme.warning
        case .error: return Theme.danger
        case .offline: return Theme.textMuted
        }
    }

    private var statusText: String {
        switch connection.status {
        case .ready: return L.devicesStatusReady
        case .connecting: return L.devicesStatusConnecting
        case .error: return L.devicesStatusError
        case .offline: return L.devicesStatusOffline
        }
    }

    private var errorText: String? {
        if case .error(let message) = connection.status { return message }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: icon)
                    .font(.title2)
                    .foregroundStyle(Theme.accent)
                    .frame(width: 40, height: 40)
                    .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                VStack(alignment: .leading, spacing: 3) {
                    Text(device.name).font(.headline).foregroundStyle(Theme.text).lineLimit(1)
                    HStack(spacing: 6) {
                        StatusDot(color: statusColor, label: statusText, pulsing: connection.status == .connecting)
                        Text(statusText).font(.subheadline).foregroundStyle(Theme.textMuted)
                        if let label = connection.transportLabel, connection.status.isReady {
                            Text("·").foregroundStyle(Theme.textMuted)
                            Text(label).font(.subheadline).foregroundStyle(Theme.textMuted).lineLimit(1)
                        }
                    }
                }
                Spacer(minLength: 0)
                if let kind = connection.activeTransport ?? device.lastTransport {
                    Chip(text: kind.label, tint: Theme.accent)
                }
            }
            if let errorText {
                Text(errorText).font(.footnote).foregroundStyle(Theme.danger).lineLimit(3)
            }
            HStack(spacing: 8) {
                if connection.status.isReady {
                    if connection.waitingCount > 0 { Chip(text: L.devicesWaiting(connection.waitingCount), icon: "hand.raised", tint: Theme.warning) }
                    if connection.runningCount > 0 { Chip(text: L.devicesRunning(connection.runningCount), icon: "circle.dotted", tint: Theme.accent) }
                    if connection.unreadCount > 0 { Chip(text: L.devicesUnread(connection.unreadCount), icon: "circle.fill", tint: Theme.accent) }
                    if connection.waitingCount == 0, connection.runningCount == 0, connection.unreadCount == 0 {
                        Text(L.devicesChats(connection.sessions.filter { !$0.archived }.count)).font(.footnote).foregroundStyle(Theme.textMuted)
                    }
                } else if let at = device.lastConnectedAt {
                    Text(L.devicesLastConnected(Format.relative(at))).font(.footnote).foregroundStyle(Theme.textMuted)
                } else {
                    Text(L.devicesNeverConnected).font(.footnote).foregroundStyle(Theme.textMuted)
                }
                Spacer()
                if let v = device.version, !v.isEmpty { Text(L.devicesVersion(v)).font(.caption2).foregroundStyle(Theme.textMuted) }
            }
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous).stroke(Theme.border))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(L.devicesA11yCard(device.name, statusText))
    }
}
