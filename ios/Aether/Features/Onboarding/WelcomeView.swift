import SwiftUI

/// First run: the mark and wordmark, one sentence, and the three ways in.
struct WelcomeView: View {
    @Environment(AppModel.self) private var model
    @State private var addDevice = false
    @State private var signIn = false

    var body: some View {
        ZStack {
            Theme.bg.ignoresSafeArea()
            VStack(spacing: 0) {
                Spacer(minLength: 24)
                VStack(spacing: 18) {
                    AetherMark(size: 96)
                        .shadow(color: Theme.accent.opacity(0.25), radius: 24, y: 8)
                    AetherWordmark(size: 34)
                    Text(L.welcomeTagline)
                        .font(.body)
                        .foregroundStyle(Theme.textMuted)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 360)
                }
                .padding(.horizontal, 32)
                Spacer(minLength: 24)
                VStack(spacing: 12) {
                    WelcomeAction(title: L.welcomeConnectDevice, hint: L.welcomeConnectHint, icon: "qrcode.viewfinder", primary: true) { addDevice = true }
                    WelcomeAction(title: L.welcomeSignIn, hint: L.welcomeSignInHint, icon: "person.crop.circle") { signIn = true }
                    WelcomeAction(title: L.welcomeDemo, hint: L.welcomeDemoHint, icon: "play.circle") { model.addDemo() }
                    Button(L.commonLater) {
                        model.preferences.hasSeenWelcome = true
                        model.showWelcome = false
                    }
                    .font(.subheadline)
                    .foregroundStyle(Theme.textMuted)
                    .padding(.top, 6)
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 28)
                .frame(maxWidth: 480)
            }
        }
        .sheet(isPresented: $addDevice) { AddDeviceView() }
        .sheet(isPresented: $signIn) {
            NavigationStack { HubAuthView(mode: .signIn, onDone: { model.showWelcome = false; model.preferences.hasSeenWelcome = true }) }
        }
    }
}

private struct WelcomeAction: View {
    var title: String
    var hint: String
    var icon: String
    var primary = false
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: icon)
                    .font(.title2)
                    .frame(width: 32)
                    .foregroundStyle(primary ? Color.white : Theme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.headline).foregroundStyle(primary ? Color.white : Theme.text)
                    Text(hint).font(.footnote).foregroundStyle(primary ? Color.white.opacity(0.85) : Theme.textMuted)
                }
                Spacer()
                Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(primary ? Color.white.opacity(0.8) : Theme.textMuted)
            }
            .padding(16)
            .background(primary ? Theme.accent : Theme.surface, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous).stroke(primary ? Color.clear : Theme.border))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(title))
        .accessibilityHint(Text(hint))
    }
}
