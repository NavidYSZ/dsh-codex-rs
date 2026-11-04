# Aether für iOS

Aether ist die iPhone-App zu dsh-codex: Sie verbindet sich mit `dsh-codex web` auf dem Mac
oder einem Server (in der App heißen diese „Geräte“), listet deren Chats, zeigt den
Verlauf live wie die Web-Oberfläche (Nachrichten, Werkzeugzeilen, Diffs, Plan,
Freigaben) und schickt neue Nachrichten. SwiftUI, iOS 17+, Swift 6 mit strikter
Concurrency; einzige Pakete: swift-nio, swift-nio-ssh (0.15.0), swift-crypto.

Vertrag: [`docs/aether/SPEC.md`](../docs/aether/SPEC.md) §2 (Verbindungswege,
Kopplungs-Payload) und §4 (App). Das Protokoll ist
[`docs/rust/SPEC.md`](../docs/rust/SPEC.md) §6; `ui/src/api.js` und `ui/src/reduce.js`
sind die Referenz, die `Aether/Core` portiert.

## Bauen und starten

```sh
cd ios
xcodegen generate            # erzeugt Aether.xcodeproj und Aether/Info.plist aus project.yml
xcodebuild -project Aether.xcodeproj -scheme Aether \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath build build
xcrun simctl boot "iPhone 17 Pro"
xcrun simctl install booted build/Build/Products/Debug-iphonesimulator/Aether.app
xcrun simctl launch booted de.yousufzai.aether
```

`project.yml` ist die Quelle; `Aether.xcodeproj` und `Aether/Info.plist` werden erzeugt
und sind nicht im Git (`.gitignore`). Nach Änderungen an `project.yml` oder neuen Dateien
erneut `xcodegen generate` ausführen.

Für ein echtes iPhone `Config/Local.xcconfig.example` nach `Config/Local.xcconfig`
kopieren und `DEVELOPMENT_TEAM` setzen (git-ignoriert). Simulator-Builds signieren ad hoc.

Tests (Kopplungs-Links, Reducer gegen die Fixtures der Web-UI, `DirectTransport` mit
gestubbtem `URLProtocol`):

```sh
xcodebuild -project Aether.xcodeproj -scheme Aether \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath build test
```

`Fixtures/*.expected.json` erzeugt `node Tools/gen-expected.mjs` aus `ui/fixtures`
(nach Änderungen an `ui/src/reduce.js` neu erzeugen). Texte stehen in
`Tools/gen-strings.py` (Deutsch zuerst) und werden mit `python3 Tools/gen-strings.py` in
`Localizable.xcstrings` und `Core/L10n/L.swift` geschrieben.

## Gegen einen lokalen Server testen

```sh
python3 tools/dev/mock_deepseek.py 18090 2 &        # Modell-Attrappe
DEEPSEEK_BASE_URL=http://127.0.0.1:18090/anthropic DEEPSEEK_API_KEY=x \
  DSHX_HOME=/tmp/aether-dev target/debug/dsh-codex web --no-open --port 3190
```

Der Simulator teilt das Loopback des Macs: In der App „Gerät hinzufügen“ → „Manuell“ →
`http://127.0.0.1:3190` und das Start-Token aus der Zeile `dsh-codex web: …?token=…`.
Unverschlüsseltes http erlaubt die App nur zu Loopback-Adressen (im Simulator
voreingestellt, auf dem iPhone unter Einstellungen → Entwickler).

## Koppeln

- **QR-Code:** Web-UI oder Mac-App → Einstellungen → „Aether App“ → mobilen Zugang
  einschalten, QR mit der iPhone-Kamera oder dem Scanner der App lesen.
- **Link:** `aether://pair#<base64url(JSON)>` (SPEC §2.1) öffnen oder unter
  „Gerät hinzufügen“ → „Link einfügen“ einfügen. Zum Testen lässt sich ein Link im
  Simulator mit `xcrun simctl openurl booted 'aether://pair#…'` öffnen.
- **Manuell:** Adresse + Token (https mit optionalem Zertifikat-Fingerabdruck, oder http
  zu Loopback) oder SSH (Host, Benutzer, ed25519-Schlüssel, Host-Schlüssel, Port von
  dsh-codex auf dem Server, optional ein Token-Befehl).
- **VPS:** auf dem Mac `dsh-codex pair --ssh oracle-vps-02 -F <ssh_config> --name "VPS 2"`
  erzeugt einen Schlüssel für das iPhone, hinterlegt ihn eingeschränkt auf dem Server
  und zeigt den QR-Code (`ssh`-Verbindungsweg, SPEC §2.2).

## Stand

Geprüft im Simulator (iPhone 17 Pro, iOS 26, Hell und Dunkel; Bilder in
`screenshots/`): Willkommen, Demo (Verlauf mit Werkzeugzeilen, Diff, Plan, Freigabe mit
„Einmal erlauben“), Kopplung per `aether://pair#…`-Link (http), manuelles Hinzufügen,
Chat-Liste, neuer Chat in einem Ordner (Ordnerwahl über `/api/fs/*`), Nachricht senden
und Antwort live über den WebSocket, Einstellungen.

Kopplung über `POST /api/mobile/pair` (https, selbst signiertes Zertifikat mit Pin) wurde
gegen einen Server aus dem zusammengeführten Stand geprüft: Pin akzeptiert, Chat und
WebSocket laufen über den TLS-Listener, ein falscher Pin wird abgelehnt, ein widerrufener
Client erscheint als „Das Token wird nicht mehr akzeptiert“ (auch live). Auf diesem Mac
war der Listener nur über Loopback (`https://127.0.0.1:7443`) erreichbar; LAN- und
Tailscale-Adressen liefen ins Leere, vermutlich weil die macOS-Firewall eingehende
Verbindungen zum nicht freigegebenen `dsh-codex` blockiert. Die App probiert alle
Adressen eines Payloads gleichzeitig.

Noch offen bzw. nicht geprüft:

- QR-Scanner (der Simulator hat keine Kamera).
- `https` über LAN/Tailscale von einem echten iPhone; `ssh`-Tunnel und `hub` sind gebaut,
  aber nicht gegen einen echten Server getestet. Der Test `testSSHTunnelAgainstLocalSSHD` ist übersprungen: Er
  bräuchte einen temporären Schlüssel in `~/.ssh/authorized_keys`. Manuell prüfen mit
  `dsh-codex pair --ssh` gegen den eigenen Mac (Entfernte Anmeldung an).
- Freigaben gegen einen echten Server (die Attrappe ruft nur `date`/`hostname` auf, die
  keine Freigabe brauchen), Foto-Anhänge, Live-Ansicht, Konto/Hub-Anmeldung.
- Englische Texte sind vorhanden, aber nicht durchgesehen.
