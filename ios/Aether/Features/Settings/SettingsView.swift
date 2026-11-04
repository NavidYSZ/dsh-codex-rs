import SwiftUI

/// Einstellungen: account (hub), appearance, follow-up rule, about, developer.
struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var auth: HubAuthView.Mode?
    @State private var verifyCode = ""
    @State private var confirmReset = false

    var body: some View {
        @Bindable var prefs = model.preferences
        NavigationStack {
            Form {
                accountSection
                Section(L.settingsAppearance) {
                    Picker(L.settingsAppearance, selection: $prefs.appearance) {
                        Text(L.settingsAppearanceSystem).tag(Preferences.Appearance.system)
                        Text(L.settingsAppearanceLight).tag(Preferences.Appearance.light)
                        Text(L.settingsAppearanceDark).tag(Preferences.Appearance.dark)
                    }
                    .pickerStyle(.segmented)
                }
                Section {
                    Picker(L.settingsFollowup, selection: $prefs.followup) {
                        Text(L.settingsFollowupSteer).tag(Preferences.Followup.steer)
                        Text(L.settingsFollowupQueue).tag(Preferences.Followup.queue)
                    }
                    // The section header already names the choice: two checkmark rows, no repeated label.
                    .pickerStyle(.inline)
                    .labelsHidden()
                } header: {
                    Text(L.settingsFollowup)
                } footer: {
                    Text(L.settingsFollowupHint)
                }
                Section(L.settingsAbout) {
                    LabeledContent(L.settingsVersion, value: Self.version)
                    Text(L.settingsAboutText).font(.footnote).foregroundStyle(Theme.textMuted)
                    DisclosureGroup(L.settingsLicenses) {
                        Text(L.settingsLicensesText).font(.footnote).foregroundStyle(Theme.textMuted)
                    }
                }
                Section {
                    TextField(L.settingsDeveloperHubURL, text: $prefs.hubURL, prompt: Text(verbatim: "https://hub.example"))
                        .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    Toggle(L.settingsDeveloperPlainHTTP, isOn: $prefs.allowPlainHTTP)
                    Button(L.settingsWelcomeAgain) { model.showWelcome = true; dismiss() }
                    Button(L.settingsDeveloperReset, role: .destructive) { confirmReset = true }
                } header: {
                    Text(L.settingsDeveloper)
                } footer: {
                    Text(L.settingsDeveloperHubHint + "\n" + L.settingsDeveloperPlainHTTPHint)
                }
            }
            .navigationTitle(L.settingsTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button(L.commonDone) { dismiss() }.bold() } }
            .sheet(item: $auth) { mode in NavigationStack { HubAuthView(mode: mode) } }
            .confirmationDialog(L.settingsDeveloperReset, isPresented: $confirmReset, titleVisibility: .visible) {
                Button(L.settingsDeveloperReset, role: .destructive) { model.removeAllDevices() }
                Button(L.commonCancel, role: .cancel) {}
            } message: {
                Text(L.settingsDeveloperResetConfirm)
            }
            .task { await model.refreshHub() }
        }
    }

    @ViewBuilder
    private var accountSection: some View {
        Section {
            if let session = model.hubSession {
                HStack(spacing: 12) {
                    Image(systemName: "person.crop.circle.fill").font(.largeTitle).foregroundStyle(Theme.accent)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(session.user.name ?? session.user.email).font(.headline)
                        Text(session.user.email).font(.footnote).foregroundStyle(Theme.textMuted)
                    }
                    Spacer()
                    if let tier = model.hubMe?.tier { Chip(text: tierName(tier.id), tint: Theme.accent, filled: true) }
                }
                if model.hubMe?.user.emailVerified == false || (model.hubMe == nil && session.user.emailVerified == false) {
                    HStack {
                        TextField(L.settingsAccountVerify, text: $verifyCode).keyboardType(.numberPad)
                        Button(L.settingsAccountVerifyNow) { Task { if await model.verify(code: verifyCode) { verifyCode = "" } } }
                            .disabled(verifyCode.count < 4 || model.hubBusy)
                    }
                    Text(L.settingsAccountVerifyHint).font(.footnote).foregroundStyle(Theme.textMuted)
                }
                if let tier = model.hubMe?.tier {
                    LabeledContent(L.settingsAccountServers(model.hubServers.count),
                                   value: tier.limits?.servers.map { L.settingsAccountLimit(String($0)) } ?? L.settingsAccountUnlimited)
                        .font(.footnote)
                }
                if let err = model.hubError { Text(err).font(.footnote).foregroundStyle(Theme.danger) }
                Button(L.settingsAccountSignOut, role: .destructive) { Task { await model.signOut() } }
            } else {
                VStack(alignment: .leading, spacing: 4) {
                    Text(L.settingsAccountSignedOut).font(.headline)
                    Text(L.settingsAccountSignedOutHint).font(.footnote).foregroundStyle(Theme.textMuted)
                }
                if model.hubClient == nil { Text(L.settingsAccountNoHub).font(.footnote).foregroundStyle(Theme.warning) }
                Button(L.settingsAccountSignIn) { auth = .signIn }.disabled(model.hubClient == nil)
                Button(L.settingsAccountSignUp) { auth = .signUp }.disabled(model.hubClient == nil)
            }
        } header: {
            Text(L.settingsAccount)
        }
        if model.hubSession != nil {
            Section {
                if model.hubServers.isEmpty {
                    Text(L.settingsAccountNoDevices).font(.footnote).foregroundStyle(Theme.textMuted)
                } else {
                    ForEach(model.hubServers) { server in
                        HStack {
                            StatusDot(color: server.online == true ? Theme.online : Theme.textMuted,
                                      label: server.online == true ? L.settingsAccountServerOnline : L.settingsAccountServerOffline)
                            VStack(alignment: .leading) {
                                Text(server.name)
                                if let h = server.hostname { Text(h).font(.footnote).foregroundStyle(Theme.textMuted) }
                            }
                            Spacer()
                            if model.store.device(server.id) != nil {
                                Text(L.settingsAccountAdded).font(.footnote).foregroundStyle(Theme.textMuted)
                            } else {
                                Button(L.settingsAccountAddServer) { model.addHubServer(server) }.font(.footnote)
                            }
                        }
                    }
                }
            } header: {
                Text(L.settingsAccountDevices)
            } footer: {
                Text(L.settingsAccountEnrollHint)
            }
        }
    }

    private func tierName(_ id: String) -> String {
        switch id {
        case "free": return L.settingsAccountTierFree
        case "pro": return L.settingsAccountTierPro
        case "team": return L.settingsAccountTierTeam
        default: return id
        }
    }

    static var version: String {
        let v = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
        let b = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "0"
        return "\(v) (\(b))"
    }
}

/// Sign in or sign up with the hub.
struct HubAuthView: View {
    enum Mode: String, Identifiable {
        case signIn, signUp
        var id: String { rawValue }
    }

    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State var mode: Mode
    var onDone: (() -> Void)?
    @State private var email = ""
    @State private var password = ""
    @State private var name = ""

    var body: some View {
        Form {
            Picker(L.settingsAccount, selection: $mode) {
                Text(L.settingsAccountSignIn).tag(Mode.signIn)
                Text(L.settingsAccountSignUp).tag(Mode.signUp)
            }
            .pickerStyle(.segmented)
            .listRowBackground(Color.clear)
            Section {
                TextField(L.settingsAccountEmail, text: $email).keyboardType(.emailAddress).textContentType(.emailAddress)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                SecureField(L.settingsAccountPassword, text: $password).textContentType(mode == .signUp ? .newPassword : .password)
                if mode == .signUp { TextField(L.settingsAccountNameOptional, text: $name).textContentType(.name) }
            } footer: {
                if mode == .signUp { Text(L.settingsAccountPasswordHint) }
            }
            if let err = model.hubError { Section { Text(err).font(.footnote).foregroundStyle(Theme.danger) } }
            if model.hubClient == nil { Section { Text(L.settingsAccountNoHub).font(.footnote).foregroundStyle(Theme.warning) } }
            Section {
                Button {
                    Task {
                        let ok = mode == .signIn ? await model.signIn(email: email, password: password)
                            : await model.signUp(email: email, password: password, name: name)
                        if ok { Haptics.success(); dismiss(); onDone?() }
                    }
                } label: {
                    HStack {
                        Text(mode == .signIn ? L.settingsAccountSignIn : L.settingsAccountSignUp).bold()
                        if model.hubBusy { Spacer(); ProgressView() }
                    }
                }
                .disabled(email.isEmpty || password.count < (mode == .signUp ? 10 : 1) || model.hubBusy || model.hubClient == nil)
            }
        }
        .navigationTitle(mode == .signIn ? L.settingsAccountSignIn : L.settingsAccountSignUp)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button(L.commonCancel) { dismiss() } } }
    }
}
