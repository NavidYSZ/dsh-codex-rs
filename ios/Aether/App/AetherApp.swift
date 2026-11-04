import SwiftUI

@main
struct AetherApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .preferredColorScheme(model.preferences.colorScheme)
                .tint(Theme.accent)
                .onOpenURL { url in model.handle(url: url) }
                .task { model.connectAll() }
                .onChange(of: scenePhase) { _, phase in
                    // Back in the foreground: every device reconnects on its own task; open chats resume their streams.
                    if phase == .active { model.connectAll() }
                }
        }
    }
}
