# Teil B: Fehler in Oberfläche und Mac-App

- **Status:** Umgesetzt
- **Stand:** 2026-11-05 19:10 (Europe/Berlin)
- **Commits:** `f4ee709` (Server), `3572491` (Web-UI), `fbccaf8` (Mac-App)

## Ziel

Der Nutzer: „Diese ganzen Bugs, die seit ein paar Tagen sind (seitdem wir auf
Aether gewechselt sind), müssen einmal alles aufgedröselt und gefixt werden.“
Dieser Teil behebt B1 bis B10 aus
[regressionen-seit-aether.md](regressionen-seit-aether.md): Web-Oberfläche
(`ui/`), Mac-App (`desktop/`) und die dazu nötigen Stellen in
`crates/dshx-server/src/mobile*.rs`, `crates/dsh-codex/src/runtime/ondemand.rs`
und `crates/dsh-codex/src/web/composer.rs`. Teil A (Server, Agent, Werkzeuge)
läuft parallel in einem eigenen Worktree.

## Ausgangslage

- **B1** `desktop/Sources/AppDelegate.swift` `applicationShouldTerminate` ruft
  sofort `manager.stopAll` (SIGTERM, nach 6 s SIGKILL) und antwortet nach
  spätestens 8 s. Es gibt keine Rückfrage, auch nicht bei laufenden Chats.
  `GET /api/sessions` liefert für offene Chats den Laufzeitstatus (`running`,
  `waiting-approval`, `idle`, `error`). Hintergrundjobs stehen nur im
  Sitzungsprotokoll (`job/registered` bzw. `job/settled` mit `data.jobId`).
  Jobs sind Kindprozesse des Servers und überleben keinen Neustart.
  `LocalConnection` kennt den Startzeitpunkt des Servers (`startedAt`).
- **B2** `McpSelection::offered` in `ondemand.rs`: Ohne eigene Wahl des Chats
  gilt das globale `always`. Ein Chat zeichnet eine Wahl nur über
  Aktivierungs- und Deaktivierungsnotizen oder die Tools im ersten
  Request-Kopf auf. Ändert sich „In neuen Chats vorausgewählt“, ändert das
  daher auch bestehende Chats ohne eigene Wahl. Die Texte `mcpPreselectHint`,
  `mcpLeadPicker` und `cnFoot` behaupten das Gegenteil.
- **B3** `ui/src/components/mobile.js`: `update()` lässt `pairing` stehen. Ein
  Code ohne Transport (`transports: []`) erscheint ohne Warnung. `behindSsh`
  vergleicht `info.platform` mit `'darwin'`, der Server meldet aber
  `std::env::consts::OS` = `macos`. Dadurch gilt auch der Mac als „hinter SSH“.
- **B4** `composer-extras.js` `applyStagedMcp` (aus `state.js` `send()` über
  `onCreate`) schickt die Startseiten-Auswahl auch an einen neuen Minimal-Chat.
  `web/composer.rs` `set_session_mcp` lehnt das ab.
- **B5** `useConnectors` lädt die Startseitenliste nur, solange
  `stagedMcp.servers == null`. Ein Server, der inzwischen gelöscht wurde, kann
  so noch eine Auswahl tragen.
- **B6** `crates/dshx-server/src/mobile.rs` `view` überschreibt `port` mit dem
  Port, auf dem gerade gelauscht wird. Bei Port 0 zeigt das Feld den
  Zufallsport, und „Sichern“ bleibt gesperrt.
- **B7** `ui/src/components/browser-settings.js` zeigt `UserChrome` immer, wenn
  der Server `userChrome` meldet, also auch auf Linux-Servern.
- **B8** `mobile.js` zeigt `dsh-codex pair --ssh <host> -F <ssh_config> --name …`.
  In der Mac-App liegt das Programm aber im Paket
  (`Aether.app/Contents/MacOS/dsh-codex`). Die Vorgaben `--remote-user deepseek`,
  `--remote-home /var/lib/dsh-codex` und `--unit deepseek-harness` passen nicht
  zu jeder Instanz.
- **B9** Das Umbenennen-Feld in `DeviceRow` setzt auf `autofocus`. Das wirkt
  bei einem später eingefügten Element nicht.
- **B10** `desktop/Sources/TransferSheet.swift` `perform()` überspringt Export
  und Import bei `sendCount == 0`. Bei einer leeren Quelle legt die App am Ziel
  deshalb keinen Ordner an, und `POST /api/workspaces` scheitert.
  `transfer.rs` `import` legt den Zielordner in `prepare_target` an. Ein leeres
  Archiv aus `export` ist gültig (tar-Endblöcke plus zstd-Rahmen).

## Plan

1. **B1:** Vor `stopAll` fragt die App jede lokale Instanz mit fertiger
   Verbindung über `InstanceAPI` (Bearer-Token aus der Seiten-URL) mit eigener
   kurzer Zeitgrenze (insgesamt etwa 3 s) ab:
   - Laufende Chats: Zeilen aus `GET /api/sessions` mit Status `running` oder
     `waiting-approval`.
   - Hintergrundjobs: Für Chats, die seit dem Serverstart geändert wurden
     (höchstens die 20 jüngsten), liest die App `GET /api/sessions/:id` und
     zählt `job/registered` seit dem Serverstart ohne passendes `job/settled`.
     `LocalConnection.startedAt` wird dafür lesbar.
   - Ist etwas aktiv, erscheint ein natives NSAlert, z. B. „2 Chats arbeiten
     noch“, mit Chatnamen, „Beenden“ und „Abbrechen“. „Abbrechen“ ist die
     Vorgabe (Return). „Abbrechen“ antwortet mit `false` und setzt
     `terminating` zurück.
   - Eine nicht erreichbare Instanz oder eine Zeitüberschreitung zählt als
     „nichts bekannt“, damit das Beenden nie hängt. Die 8-s-Sicherung gilt
     erst ab dem eigentlichen Herunterfahren. SSH- und URL-Instanzen laufen
     weiter und werden nicht gefragt.
2. **B2:**
   - Neue Chats bekommen im Sitzungskopf `mcpPreselected` mit den Servern, die
     beim Anlegen `always` hatten (`Runtime::create_session`, nicht für
     Minimal). Das gilt serverseitig für Web, iOS und API.
   - `McpSelection` bekommt `preselected` (aus dem Kopf). `preselects(server)`
     liefert die Liste des Chats, bei älteren Chats ohne Liste weiter
     `always`. `offered` nutzt das. Auch Systemprompt-Abschnitte und die
     Aktivierungsnotizen in `McpHook` richten sich danach statt nach dem
     aktuellen `always`.
   - Ältere Chats verhalten sich unverändert, damit sich ihre Tool-Sets nicht
     nebenbei ändern.
   - Verzweigungen (`fork.rs` `fork_header`) übernehmen die Liste wie
     `agentPreset`.
   - Die Aufrufer reichen den Kopf durch: `runtime.rs`, `web/composer.rs` und
     `runtime/suggest.rs`.
   - Texte (DE/EN): Eine Änderung wirkt nur auf neue Chats. Chats von vor
     diesem Update ohne eigene Wahl folgen dem Schalter weiter. Dasselbe gilt
     für die README-Absätze und `docs/rust/SPEC.md`.
3. **B3:**
   - `update()` verwirft den angezeigten Code, und ein nie benutzter Code wird
     widerrufen, wie schon beim Neu-Erzeugen.
   - Hat die Antwort keine Transporte, erscheint eine Warnung am Code.
   - Die Mac-Erkennung akzeptiert `macos` und `darwin`. Zusätzlich gilt eine
     SSH-Instanz der Mac-App als „hinter SSH“.
   - Hinter SSH steht die SSH-Karte zuerst. Die QR-Karte erklärt dann, dass sie
     den direkten Zugang braucht.
4. **B4/B5:**
   - `applyStagedMcp` fragt zuerst `GET /api/sessions/:id/mcp` des neuen Chats.
     Bei `minimal` oder lokalem Minimal-Modus verwirft es die Auswahl ohne
     Anfrage.
   - Es schickt nur Auswahlen für Server, die der Chat noch kennt und die vom
     Chatstand abweichen.
   - `useConnectors` lädt die Liste bei jedem Einblenden der Startseite neu,
     dazu kommt eine laufende Anfrage, die mehrfaches Laden verhindert.
   - `loadStaged` verwirft Auswahlen für verschwundene Server und Auswahlen,
     die der neuen Vorauswahl entsprechen.
   - Die MCP-Einstellungen (Vorauswahl, Hinzufügen, Entfernen) verwerfen die
     Startseitenliste.
5. **B6:** `GET /api/mobile` liefert zusätzlich `configuredPort` (gespeichert).
   `port` bleibt wie bisher der laufende Port, damit die iOS-App nichts
   verliert. Das Feld und „Sichern“ nutzen `configuredPort`. Der laufende Port
   steht als Hinweis daneben, wenn er abweicht.
6. **B7:** Die Karte „Dein eigenes Chrome“ erscheint nur bei `info.platform`
   `macos` (Fixture: `darwin`).
7. **B8:** `GET /api/mobile` liefert `sshPair: {user, home, bin, port, unit}`
   dieses Servers:
   - Benutzer über `/proc/self/status` und `/etc/passwd`, sonst `$USER`.
   - `home()` des Hosts.
   - `current_exe` ohne „ (deleted)“.
   - Loopback-Port.
   - systemd-Unit aus `/proc/self/cgroup`.

   Die Mac-Brücke meldet `cliPath`, also den Pfad des mitgelieferten
   Programms, und bei SSH-Instanzen `ssh: {host, config}`. Die Karte setzt
   daraus einen Befehl zusammen:
   - Programmpfad, sonst `/Applications/Aether.app/Contents/MacOS/dsh-codex`.
   - Host und Konfiguration, sonst Platzhalter `<ssh-host>` und
     `<ssh_config>`.
   - `--remote-user`, `--remote-home` und `--unit`, sonst Platzhalter.
     Platzhalter in spitzen Klammern lassen die Shell sofort scheitern statt
     falsch zu laufen.
   - `--remote-port` und `--remote-bin` nur bei Abweichung von der Vorgabe.

   Dazu kommen ein Kopierknopf und ein Hinweis auf die Platzhalter. Die
   Brücke wird in `desktop/README.md` dokumentiert, die API in
   `docs/aether/SPEC.md`.
8. **B9:** Das Umbenennen-Feld bekommt einen `ref` und wird per `useEffect`
   fokussiert und markiert.
9. **B10:** Fehlt der Zielordner (`plan.existing == nil`), laufen Export und
   Import auch ohne Einträge. Das leere Archiv legt den Ordner über
   `prepare_target` an. Die Abschlussmeldung nennt den neu angelegten leeren
   Ordner. `transfer.rs` bleibt unverändert.

Bewusst ausgelassen:

- Keine neue Server-Route für Jobs, weil der Teil-A-Bereich (`web.rs`) nicht
  berührt wird. Die Mac-App zählt Jobs aus den Protokollen.
- `CHANGELOG.md` schreibt der Nutzer beim Zusammenführen.
- Keine Tests, Smoke-Tests oder Lints nach Nutzerwunsch.

## Umsetzung

### B1 – Rückfrage beim Beenden

Geändert: `desktop/Sources/AppDelegate.swift` und `Connections.swift`.

- `applicationShouldTerminate` antwortet `.terminateLater` und startet
  `QuitCheck.run(manager)`.
- `QuitCheck` nimmt jede lokale Instanz mit fertiger `LocalConnection` und
  prüft zwei Dinge, mit eigener `URLSession` und 1,5 s pro Anfrage:
  - Zeilen aus `GET /api/sessions` mit `running` oder `waiting-approval`.
  - Höchstens 20 seit `startedAt` geänderte Chats über
    `GET /api/sessions/:id`. Gezählt werden `job/registered` (Zeit ≥ Serverstart)
    ohne `job/settled`, Schlüssel `data.jobId`.
- Danach folgt ein NSAlert, z. B. „2 Chats arbeiten noch“ oder
  „1 Chat arbeitet noch, 1 Hintergrundjob läuft“. Es nennt bis zu sechs
  Chatnamen mit Instanz. „Beenden“ ist als destruktiv markiert, „Abbrechen“
  liegt auf Return.
- „Abbrechen“ antwortet `false` und setzt `terminating` zurück. „Beenden“
  oder eine leere Prüfung führen zu `shutDown()`, dem bisherigen `stopAll`
  mit der 8-s-Sicherung.
- `LocalConnection.startedAt` ist jetzt `private(set)`.
- Im Protokoll stehen nur Anzahlen, keine Titel.

### B2 – Vorauswahl nur für neue Chats

Geändert: `crates/dsh-codex/src/runtime/ondemand.rs`, `runtime.rs`,
`web/composer.rs`, `runtime/suggest.rs` und `fork.rs`.

- `ondemand.rs`:
  - Neu sind `PRESELECTED = "mcpPreselected"`, `preselection(header)`,
    `record_preselection` und `Runtime::preselected_servers()`. Letzteres
    liefert alle konfigurierten, laufenden und Plugin-Server mit `always`.
  - `McpHistory.preselected` und `McpSelection.preselected` sind neu. Ohne
    eigene Wahl entscheidet `McpSelection::preselects`, nicht mehr das
    aktuelle `always`.
  - `mcp_prompt_sections` nimmt nur noch ein Prädikat. Chat-Abschnitte und
    `McpHook` geben `preselects && offered` hinein.
  - Die Entscheidung „Anweisungen in der Notiz oder im Systemprompt“ folgt
    `preselects`.
  - `McpHook` trägt die Vorauswahl aus dem Kopf.
- `runtime.rs`:
  - `create_session` schreibt `mcpPreselected` für alle Modi außer Minimal.
  - `session_tools`, `refresh_agent`, `tools_for` und `new_agent` reichen den
    Kopf durch. `agent.header()` und `log.header()` sind dafür schon
    vorhanden.
- `web/composer.rs` und `runtime/suggest.rs` geben `header` weiter.
  `SuggestContext.session` ist jetzt `(id, header, events)`.
- `fork.rs`: `fork_header` übernimmt `mcpPreselected` wie `agentPreset`.
- Chats ohne den Schlüssel verhalten sich genau wie vorher. Kein bestehender
  Chat ändert dadurch sein Tool-Set.
- Der Weg der ACP-SDK-Sitzungen (`sdk.rs`) legt den Kopf selbst an und bleibt
  beim bisherigen Verhalten.
- Texte: `mcpPreselectHint` und `mcpLeadPicker` (DE/EN) sagen jetzt, dass
  bestehende Chats ihre Server behalten. Nur Chats von vor dem Update ohne
  eigene Wahl folgen dem Schalter.
- `browserUse.mcpAlways` heißt jetzt „In neuen Standard- und PTC-Chats
  vorausgewählt“.
- `cnFoot` stimmt mit dem neuen Verhalten und bleibt.
- README, README.de und `docs/rust/SPEC.md` sind ergänzt.

### B3, B6, B8, B9 – Seite „Aether-App“

Geändert: `ui/src/components/mobile.js` und
`crates/dshx-server/src/mobile.rs`.

- B3:
  - `update()` ruft `dropPairing()` auf. Das verwirft den angezeigten Code und
    widerruft einen nie gesehenen Client, wie zuvor nur beim Neu-Erzeugen.
  - Ohne `payload.transports` erscheint die Warnung `mobileNoTransport`, bzw.
    `mobileNoTransportSsh` hinter SSH.
  - Die Mac-Erkennung `isMacPlatform` (in `state.js`) akzeptiert `macos` und
    `darwin`. „Hinter SSH“ heißt jetzt: SSH-Instanz der Mac-App, oder
    Plattform kein Mac.
  - Hinter SSH steht die SSH-Karte oben. Die QR-Karte heißt dann „iPhone
    direkt verbinden“ und hat einen Hinweis. Die Warnung zum ausgeschalteten
    Zugang erscheint auch dort.
- B6:
  - `view` liefert `configuredPort`. `port` bleibt der laufende Port, wichtig
    für ältere Clients.
  - Das Feld und „Sichern“ nutzen `configuredPort`. Weicht der laufende Port
    ab, steht `mobilePortActive` im Hinweis.
- B8, Server: `view` liefert `sshPair`:
  - `user` aus `/proc/self/status` (effektive UID) und `/etc/passwd`, sonst
    `$USER`.
  - `home` aus `MobileHost::home()`.
  - `bin` aus `current_exe` ohne „ (deleted)“.
  - `port` ist der Loopback-Port.
  - `unit` ist die `.service`-Komponente aus `/proc/self/cgroup`.
- B8, Oberfläche:
  - `sshPairCommand()` setzt den Befehl zusammen.
  - Programm: `dshxDesktop.cliPath`, sonst
    `/Applications/Aether.app/Contents/MacOS/dsh-codex`.
  - `--ssh` und `-F` kommen aus `dshxDesktop.ssh`, sonst `<ssh-host>` und
    `<ssh_config>`.
  - `--name` ist der Servername.
  - `--remote-user`, `--remote-home` und `--unit` sind immer dabei, sonst als
    Platzhalter.
  - `--remote-port` und `--remote-bin` stehen nur bei Abweichung von 3080 bzw.
    `/usr/local/bin/dsh-codex` im Befehl.
  - Werte werden für die Shell gequotet. Platzhalter bleiben in spitzen
    Klammern, die Shell bricht dann ab.
  - Dazu gibt es einen Kopierknopf und den Hinweis `mobileSshPlaceholders`.
- B8, Mac-App: Die Brücke hat neu `cliPath` (aus
  `Bundle.main.path(forAuxiliaryExecutable:)`) und bei SSH-Instanzen
  `ssh: {host, config}`. Geändert sind `MainWindow.swift` `bridgeState` und
  `Bridge.swift`. Dokumentiert ist das in `desktop/README.md`.
- Die Fixture in `ui/src/api.js` kennt `configuredPort`, `sshPair`, Port 0 und
  HTTPS-Transporte bei eingeschaltetem Zugang.
- B9: `DeviceRow` fokussiert und markiert das Feld per `useRef`/`useEffect`,
  sobald das Umbenennen beginnt.

### B4, B5 – Konnektoren der Startseite

Geändert: `ui/src/components/composer-extras.js` und `ui/src/state.js`.

- `loadStaged()` lädt bei jedem Einblenden der Startseite (`useEffect` auf
  `[id]`, ohne die frühere `servers == null`-Sperre) und beim Öffnen des
  Pickers. Eine laufende Anfrage wird geteilt.
- `livePicks` verwirft Auswahlen für nicht mehr gelistete Server und
  Auswahlen, die der Vorauswahl entsprechen.
- Ein eigenes Verwerfen nach MCP-Einstellungsänderungen war unnötig. Seiten
  wie Plugins oder Einstellungen hängen die Startseite aus, und die lädt beim
  Zurückkommen neu.
- `send()` gibt `onCreate(id, {preset})` mit.
- `applyStagedMcp` verwirft die Auswahl bei Minimal ohne Anfrage. Es fragt
  `GET /api/sessions/:id/mcp`, bei `minimal: true` endet es dort. Es schickt
  nur Auswahlen für Server, die der neue Chat listet und deren `selected`
  abweicht. Ältere Server ohne `servers`: Rückfall auf die Startseitenzeilen.

### B7 – Karte „Dein eigenes Chrome“

`browser-settings.js` zeigt `UserChrome` nur bei `isMacPlatform(info.platform)`.

### B10 – Leeren Ordner übertragen

`TransferSheet.swift` `perform()`:

- Fehlt der Zielordner, laufen Export und Import auch bei `sendCount == 0`
  (`createsEmpty`). Das leere tar.zst aus `/api/transfer/export` legt den
  Ordner über `prepare_target` an.
- Danach folgen Prüfung und `POST /api/workspaces` wie gewohnt.
- Die Meldung lautet „Leeren Ordner … angelegt.“
- `transfer.rs` bleibt unverändert.

### Abweichungen vom Plan

- Kein Verwerfen der Startseitenliste aus `mcp.js` heraus, Grund siehe B5.
- Die Übersicht in `docs/planning/README.md` steht noch auf „In Arbeit“. Sie
  wird beim Zusammenführen mit Teil A gepflegt, um Konflikte in derselben
  Tabelle zu vermeiden.

## Prüfung

Auf Nutzerwunsch keine Smoke-Tests, Testsuiten oder Lints. Geprüft wurde am
2026-11-05:

- `node --check` für `mobile.js`, `composer-extras.js`, `browser-settings.js`,
  `state.js`, `i18n.js` und `api.js`: in Ordnung.
- `node --test ui/dev/composer_check.mjs`: 16 von 16 bestanden.
- `cargo check -p dsh-codex -p dshx-server` (gemeinsamer `target`): ohne Fehler
  und Warnungen.
- `rustfmt --edition 2024 --check` für die berührten Rust-Dateien: in Ordnung.
- `xcrun swiftc -typecheck -swift-version 5 -target arm64-apple-macos14.0
  desktop/Sources/*.swift`: ohne Fehler und Warnungen.
- Per Abgleich: Alle neuen und benutzten `mobile*`-Texte stehen in DE und EN.

Nicht geprüft:

- Der Testcode in `ondemand.rs` ist nicht übersetzt (`cargo check` ohne
  `--tests`): das ergänzte Feld `preselected` und die neue Zusicherung zur
  gespeicherten Vorauswahl.
- Nicht gebaut und nicht ausprobiert: App-Paket, Rückfrage-Dialog,
  Übertragung, Kopplungsseite und Startseite im Browser.
- `browser_settings_dom_check.mjs` braucht Chrome und lief nicht.

## Offen

- Alles einmal in der gebauten App testen:
  - Beenden mit laufendem Chat und mit Hintergrundjob.
  - Leeren Ordner kopieren.
  - Kopplungsseite auf einer VPS-Instanz: Befehl, Platzhalter, Port 0.
  - Startseite mit Minimal und einem gelöschten Server.
- Die Jobzählung beim Beenden liest ganze Chatprotokolle. Ein sehr großes
  Protokoll kann die 1,5 s überschreiten und zählt dann nicht. Eine kleine
  Server-Route mit Job-Anzahlen (Teil-A-Bereich `web.rs`) wäre genauer und
  billiger.
- Die `docs/planning/README.md`-Übersicht beim Zusammenführen auf „Umgesetzt“
  setzen. `CHANGELOG.md` schreibt der Nutzer.
- Die alte Fehlererkennung `info.platform !== 'darwin'` hatte auch auf dem Mac
  „hinter SSH“ ergeben. Das ist mit B3 behoben, sollte aber im Changelog
  stehen.
