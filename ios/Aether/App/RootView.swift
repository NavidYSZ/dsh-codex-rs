import SwiftUI

/// Devices → chats → chat as a split view: three columns on iPad, a stack on iPhone.
struct RootView: View {
    @Environment(AppModel.self) private var model
    @State private var columns = NavigationSplitViewVisibility.all

    var body: some View {
        @Bindable var model = model
        Group {
            if model.showWelcome {
                WelcomeView()
            } else {
                NavigationSplitView(columnVisibility: $columns) {
                    DevicesView()
                } content: {
                    if let id = model.selectedDeviceID, let connection = model.connection(for: id) {
                        ChatsView(connection: connection)
                            .id(id)
                    } else {
                        ContentUnavailableView(L.devicesTitle, systemImage: "laptopcomputer.and.iphone", description: Text(L.devicesEmptyText))
                    }
                } detail: {
                    if let chat = model.selectedChat, let connection = model.connection(for: chat.deviceID) {
                        ChatScreen(ref: chat, connection: connection)
                            .id(chat)
                    } else {
                        ContentUnavailableView(L.chatsTitle, systemImage: "bubble.left.and.bubble.right", description: Text(L.chatsEmptyText))
                    }
                }
                .navigationSplitViewStyle(.balanced)
            }
        }
        .sheet(isPresented: $model.showSettings) { SettingsView() }
        .sheet(isPresented: $model.showAddDevice) { AddDeviceView() }
        .sheet(item: $model.incomingPairing) { payload in
            PairingConfirmView(payload: payload, fromLink: true)
        }
        .alert(L.addInvalidLink, isPresented: Binding(get: { model.incomingPairingError != nil }, set: { if !$0 { model.incomingPairingError = nil } })) {
            Button(L.commonOk, role: .cancel) {}
        } message: {
            Text(model.incomingPairingError ?? "")
        }
    }
}

extension PairingPayload: Identifiable {
    var id: String { server.id }
}
