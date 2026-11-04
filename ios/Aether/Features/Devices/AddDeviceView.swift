import CryptoKit
import SwiftUI

/// Gerät hinzufügen: scan the QR code, paste the link, or enter the connection by hand.
struct AddDeviceView: View {
    enum Mode: String, CaseIterable, Identifiable {
        case scan, paste, manual
        var id: String { rawValue }
        var title: String {
            switch self {
            case .scan: return L.addScan
            case .paste: return L.addPaste
            case .manual: return L.addManual
            }
        }
    }

    @Environment(\.dismiss) private var dismiss
    @State private var mode: Mode = QRScanner.isSupported ? .scan : .paste
    @State private var pasted = ""
    @State private var error: String?
    @State private var found: PairingPayload?

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                Picker(L.addTitle, selection: $mode) {
                    ForEach(Mode.allCases) { m in Text(m.title).tag(m) }
                }
                .pickerStyle(.segmented)
                .padding(.horizontal)
                .padding(.top, 8)
                .padding(.bottom, 12)
                switch mode {
                case .scan: scanPane
                case .paste: pastePane
                case .manual: ManualEntryView(onPayload: { found = $0 })
                }
            }
            .background(Theme.bg)
            .navigationTitle(L.addTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(L.commonCancel) { dismiss() } }
            }
            .sheet(item: $found) { payload in
                PairingConfirmView(payload: payload, fromLink: false) { dismiss() }
            }
        }
    }

    private var scanPane: some View {
        VStack(spacing: 14) {
            if QRScanner.isSupported {
                QRScannerView { text in handle(text) }
                    .clipShape(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous).stroke(Theme.border))
                    .padding(.horizontal)
                    .frame(maxHeight: 420)
                Text(L.addScanHint).font(.footnote).foregroundStyle(Theme.textMuted).multilineTextAlignment(.center).padding(.horizontal, 24)
            } else {
                ContentUnavailableView(L.addNoCamera, systemImage: "camera.fill")
                    .frame(maxHeight: 260)
            }
            pasteField
            Spacer()
        }
    }

    private var pastePane: some View {
        VStack(spacing: 14) {
            Text(L.addPasteHint).font(.footnote).foregroundStyle(Theme.textMuted).multilineTextAlignment(.center).padding(.horizontal, 24)
            pasteField
            Spacer()
        }
    }

    private var pasteField: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField(L.addPastePlaceholder, text: $pasted, axis: .vertical)
                .lineLimit(2...5)
                .textFieldStyle(.plain)
                .font(.footnote.monospaced())
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .padding(10)
                .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
                .accessibilityLabel(L.addPaste)
                .onSubmit { handle(pasted) }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.danger)
            }
            HStack {
                Button {
                    if let s = UIPasteboard.general.string, !s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        pasted = s
                        handle(s)
                    } else {
                        error = L.addClipboardEmpty
                    }
                } label: { Label(L.addPasteFromClipboard, systemImage: "doc.on.clipboard") }
                .buttonStyle(.bordered)
                Spacer()
                Button(L.commonContinue) { handle(pasted) }
                    .buttonStyle(.borderedProminent)
                    .disabled(pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(.horizontal)
    }

    private func handle(_ text: String) {
        do {
            found = try PairingPayload.parse(text)
            error = nil
            Haptics.light()
        } catch let e as PairingError {
            error = e == .notALink ? L.addInvalidLink : L.addInvalidPayload
        } catch {
            self.error = L.addInvalidPayload
        }
    }
}

/// The parsed payload before it is stored: what was found, which ways it offers, and "Verbinden".
struct PairingConfirmView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let payload: PairingPayload
    var fromLink: Bool
    var onConnected: (() -> Void)?

    private var existing: Device? { model.store.device(payload.server.id) }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack(spacing: 12) {
                        Image(systemName: ["darwin", "macos"].contains(payload.server.platform ?? "") ? "laptopcomputer" : "server.rack")
                            .font(.title2).foregroundStyle(Theme.accent).frame(width: 36)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(payload.server.name.isEmpty ? (payload.server.hostname ?? payload.server.id) : payload.server.name).font(.headline)
                            if let h = payload.server.hostname { Text(h).font(.footnote).foregroundStyle(Theme.textMuted) }
                            if let v = payload.server.version { Text(L.devicesVersion(v)).font(.footnote).foregroundStyle(Theme.textMuted) }
                        }
                    }
                } header: {
                    Text(fromLink ? L.addFromLink : L.addFound(payload.server.name))
                }
                Section(L.addTransports) {
                    ForEach(Array(payload.transports.enumerated()), id: \.offset) { _, t in
                        if let stored = StoredTransport(transport: t) {
                            HStack {
                                Chip(text: stored.kind.label, tint: Theme.accent)
                                Text(stored.summary).font(.footnote).foregroundStyle(Theme.textMuted).lineLimit(1)
                            }
                        }
                    }
                }
                if let existing {
                    Section { Text(L.addReplace(existing.name)).font(.footnote).foregroundStyle(Theme.warning) }
                }
            }
            .navigationTitle(L.addPairTitle)
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(L.commonCancel) { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L.commonConnect) {
                        let device = model.add(payload: payload)
                        // Show the new device's chats (on iPhone this leaves whatever chat was open).
                        model.selectedChat = nil
                        model.selectedDeviceID = device.id
                        Haptics.success()
                        dismiss()
                        onConnected?()
                    }
                    .bold()
                }
            }
        }
    }
}

/// Manual entry: address + token, or an SSH connection.
struct ManualEntryView: View {
    enum Kind: String, CaseIterable, Identifiable {
        case urlToken, ssh
        var id: String { rawValue }
    }

    var onPayload: (PairingPayload) -> Void

    @State private var kind: Kind = .urlToken
    @State private var name = ""
    @State private var url = ""
    @State private var token = ""
    @State private var certSha256 = ""
    @State private var host = ""
    @State private var port = "22"
    @State private var user = "ubuntu"
    @State private var hostKey = ""
    @State private var privateKey = ""
    @State private var remotePort = "3080"
    @State private var tokenCommand = ""
    @State private var error: String?

    var body: some View {
        Form {
            Picker(L.addManual, selection: $kind) {
                Text(L.addManualUrlToken).tag(Kind.urlToken)
                Text(L.addManualSsh).tag(Kind.ssh)
            }
            .pickerStyle(.segmented)
            .listRowBackground(Color.clear)
            Section {
                TextField(L.addManualName, text: $name)
            }
            switch kind {
            case .urlToken:
                Section {
                    TextField(L.addManualUrl, text: $url, prompt: Text(L.addManualUrlPlaceholder))
                        .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    SecureField(L.addManualToken, text: $token)
                    TextField(L.addManualCertSha256, text: $certSha256).font(.footnote.monospaced()).textInputAutocapitalization(.never).autocorrectionDisabled()
                }
            case .ssh:
                Section {
                    TextField(L.addManualHost, text: $host).keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    TextField(L.addManualPort, text: $port).keyboardType(.numberPad)
                    TextField(L.addManualUser, text: $user).textInputAutocapitalization(.never).autocorrectionDisabled()
                    TextField(L.addManualHostKey, text: $hostKey).font(.footnote.monospaced()).textInputAutocapitalization(.never).autocorrectionDisabled()
                    TextField(L.addManualRemotePort, text: $remotePort).keyboardType(.numberPad)
                }
                Section(L.addManualKey) {
                    TextEditor(text: $privateKey).font(.caption.monospaced()).frame(minHeight: 120).textInputAutocapitalization(.never).autocorrectionDisabled()
                    Button(L.addManualPasteKey) { if let s = UIPasteboard.general.string { privateKey = s } }
                }
                Section {
                    TextField(L.addManualTokenCommand, text: $tokenCommand).font(.footnote.monospaced()).textInputAutocapitalization(.never).autocorrectionDisabled()
                    SecureField(L.addManualToken, text: $token)
                }
            }
            if let error { Section { Text(error).foregroundStyle(Theme.danger).font(.footnote) } }
            Section {
                Button(L.commonContinue) { submit() }.bold()
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.bg)
    }

    private func submit() {
        let trimmedName = name.trimmingCharacters(in: .whitespaces)
        switch kind {
        case .urlToken:
            let address = url.trimmingCharacters(in: .whitespacesAndNewlines)
            guard let u = URL(string: address), let scheme = u.scheme?.lowercased(), let h = u.host, scheme == "http" || scheme == "https" else {
                error = L.addInvalidURL
                return
            }
            guard !token.trimmingCharacters(in: .whitespaces).isEmpty else { error = L.addMissingFields; return }
            let id = "srv_manual_" + Self.shortHash("\(h):\(u.port ?? 0)")
            let transport: PairingTransport = scheme == "https"
                ? .https(.init(urls: [address], certSha256: certSha256.trimmingCharacters(in: .whitespaces).isEmpty ? nil : certSha256.trimmingCharacters(in: .whitespaces).lowercased()))
                : .http(.init(urls: [address]))
            let payload = PairingPayload(server: .init(id: id, name: trimmedName.isEmpty ? h : trimmedName, hostname: h), client: nil,
                                         token: token.trimmingCharacters(in: .whitespaces), transports: [transport])
            error = nil
            onPayload(payload)
        case .ssh:
            let h = host.trimmingCharacters(in: .whitespaces)
            let u = user.trimmingCharacters(in: .whitespaces)
            let hk = hostKey.trimmingCharacters(in: .whitespaces)
            guard !h.isEmpty, !u.isEmpty, !hk.isEmpty, !privateKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  let p = Int(port.trimmingCharacters(in: .whitespaces)), let rp = Int(remotePort.trimmingCharacters(in: .whitespaces)) else {
                error = L.addMissingFields
                return
            }
            do { _ = try OpenSSHKey.ed25519PrivateKey(pem: privateKey) } catch { self.error = L.errorBadKey; return }
            let id = "srv_manual_" + Self.shortHash("ssh:\(u)@\(h):\(p)")
            let cmd = tokenCommand.trimmingCharacters(in: .whitespacesAndNewlines)
            let tok = token.trimmingCharacters(in: .whitespaces)
            let transport = PairingTransport.ssh(.init(host: h, port: p, user: u, hostKeySha256: hk, privateKey: privateKey, remoteHost: "127.0.0.1",
                                                       remotePort: rp, tokenCommand: cmd.isEmpty ? nil : cmd))
            let payload = PairingPayload(server: .init(id: id, name: trimmedName.isEmpty ? h : trimmedName, hostname: h, platform: "linux"), client: nil,
                                         token: tok.isEmpty ? nil : tok, transports: [transport])
            error = nil
            onPayload(payload)
        }
    }

    private static func shortHash(_ s: String) -> String {
        SHA256.hash(data: Data(s.utf8)).prefix(6).map { String(format: "%02x", $0) }.joined()
    }
}
