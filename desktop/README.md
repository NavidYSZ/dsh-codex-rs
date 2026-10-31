# Aether für macOS (Desktop-App)

Native macOS-App (Swift, AppKit + WKWebView, kein Electron/Tauri) um die
dsh-codex-Weboberfläche. Sie startet den Agenten lokal auf dem Mac, verbindet
sich auf Wunsch per SSH-Tunnel mit entfernten dsh-codex-Servern (z. B. VPS 2)
und schaltet zwischen diesen **Instanzen** um. Die Web-UI bleibt dieselbe; die
App ergänzt Fensterrahmen, Menüs, Mitteilungen, Dock-Badge und eine kleine
JavaScript-Brücke (`window.dshxDesktop`).

```
Aether.app/Contents/
  MacOS/dsh-codex-desktop          die App (Swift)
  MacOS/dsh-codex                  Release-Binary aus diesem Repo
  MacOS/node                       gepinnte Node-Laufzeit für PTC
  Helpers/DSH Computer Use.app     Computer-Use-Helfer aus helper/
  Resources/AppIcon.icns           aus ui/favicon.svg abgeleitet (Tools/make-icon.swift)
```

## Bauen

Voraussetzungen: Apple Silicon, Xcode 26.5 oder neuer (macOS-26-SDK), Rust
1.97.1 und Python 3. Die fertige App läuft weiterhin ab macOS 14. Der neuere
SDK wird für die optionale `NSGlassEffectView`-Darstellung benötigt.

```bash
desktop/build.sh
```

Ergebnis: `desktop/build/Aether.app` und `desktop/build/Aether.dmg`.

| Option | Wirkung |
|---|---|
| `--skip-cargo` | vorhandenes `target/release/dsh-codex` verwenden |
| `--no-dmg` | nur das App-Paket bauen |
| `--no-sign` | ohne Signatur |
| `--ad-hoc` | lokale Signatur ohne Entwickleridentität |

Ablauf: `cargo build --release -p dsh-codex` → `helper/build.sh` (Helfer, gleiche
Identität) → `xcrun swiftc` (arm64, macOS 14+) → Symbol → Signatur → `hdiutil`.
Signiert wird mit `DSH_DESKTOP_SIGN_IDENTITY`, sonst der ersten „Apple
Development“-Identität im Schlüsselbund, sonst ad hoc; innere Programme zuerst,
mit Hardened Runtime. **Keine Notarisierung**: Auf diesem Mac startet die App
normal; auf anderen Macs blockiert Gatekeeper sie (Rechtsklick → „Öffnen“ oder
Developer-ID + Notarisierung nachrüsten). Nur Apple Silicon (arm64).

Installieren: `.dmg` öffnen und `Aether.app` nach „Programme“ ziehen.

Node.js 26.13.0 wird mit verifizierter SHA-256 in die App gepackt. Die
PTC-Laufzeit wird neben der Rust-Binärdatei gefunden und benötigt kein Node
im PATH des Anwenders. Lizenz: `Contents/Resources/NODE-LICENSE.txt`.

## Lokaler Server („Dieser Mac“)

Die App startet beim Anzeigen der lokalen Instanz

```
Contents/MacOS/dsh-codex web --no-open --port 0 --ready-file ~/Library/Application Support/dsh-codex/run/local-<id>.json
```

- **`--ready-file <pfad>`** (neu in `crates/dsh-codex`): Sobald der Server
  lauscht, schreibt dsh-codex atomar (temporäre Datei + `rename`) mit Modus
  `0600` das JSON `{"url": "http://127.0.0.1:<port>/?token=<token>", "token":
  "<token>", "port": <port>}`. Beim Beenden wird die Datei entfernt. Die App
  wartet bis zu 120 s auf die Datei (MCP-Server starten vorher).
- **SIGTERM** beendet `dsh-codex web` jetzt geordnet (Agenten, MCP-Server,
  Browser), wie bisher Strg-C. Die App sendet SIGTERM beim Beenden, nach 6 s
  SIGKILL. Eine PID-Datei neben der Ready-Datei räumt nach einem Absturz einen
  verwaisten Server auf (nur wenn dessen Programmpfad passt).
- Umgebung: die Umgebung der Login-Shell des Nutzers (`$SHELL -i -l -c env`,
  Zeitlimit 8 s), damit Werkzeuge denselben `PATH` sehen wie im Terminal;
  zusätzlich `DSHX_DESKTOP=<Version>`, optional `DSHX_HOME`.
- Absturz: bis zu drei automatische Neustarts (1,5 s, 3 s, 4,5 s), danach
  Fehleranzeige mit „Erneut verbinden“.
- Die Ausgabe landet in `~/Library/Logs/dsh-codex/local-<id>.log`; Tokens werden
  vor dem Schreiben maskiert (`token=…`).

Der Computer-Use-Helfer wird beim ersten Start nach
`~/Applications/DSH Computer Use.app` kopiert, falls dort keiner liegt (dort sucht
dsh-codex ihn, und dort bleiben die macOS-Freigaben hängen). Ein vorhandener
Helfer wird nie still ersetzt; „Hilfe → Computer-Use-Helfer installieren …“
ersetzt ihn auf Nachfrage.

## Instanzen

Gespeichert in `~/Library/Application Support/dsh-codex/instances.json` (Ordner
`0700`, Datei `0600`). Fehlt die Datei, legt die App ausschließlich „Dieser Mac“ an. Bearbeiten über **Instanz → Instanzen bearbeiten …** (Liste, Reihenfolge,
Hinzufügen/Bearbeiten/Entfernen, Editor für „In Editor öffnen“) oder direkt in
der Datei (App vorher beenden).

```json
{
  "version": 1,
  "current": "local",
  "editor": "com.microsoft.VSCode",
  "instances": [
    { "id": "local", "name": "Dieser Mac", "kind": "local" },
    {
      "id": "server", "name": "Mein Server", "kind": "ssh",
      "sshConfig": "/Users/you/.ssh/config",
      "host": "my-server", "remotePort": 3080, "localPort": 3082,
      "tokenCommand": "sudo -n journalctl -u deepseek-harness -b -o cat --no-pager | grep -E '^dsh(-codex)? web: http://127\\.0\\.0\\.1:3080/\\?token=' | tail -n 1"
    },
    { "id": "…", "name": "Labor", "kind": "url", "url": "http://127.0.0.1:3090/", "connectAtLaunch": false }
  ]
}
```

| Typ | Felder | Verhalten |
|---|---|---|
| `local` | `binary`, `workingDirectory`, `dataDirectory` (alle optional) | Startet das mitgelieferte (oder angegebene) Programm, Arbeitsordner Standard `~`, Daten Standard `~/.dsh-codex`. |
| `ssh` | `sshConfig`, `host`, `remotePort`, `localPort`, `tokenCommand` | Tunnel `127.0.0.1:<localPort>` → `127.0.0.1:<remotePort>` auf dem Host, Token per SSH. |
| `url` | `url`; Token im Schlüsselbund | Prüft `GET /api/info` und lädt `url?token=…`. |

`current` ist die zuletzt gewählte Instanz, `editor` eine Bundle-ID oder ein
App-Pfad (leer: VS Code, Cursor, Zed, Sublime Text, Nova, BBEdit, TextEdit – die
erste installierte). Die Reihenfolge der Liste bestimmt ⌃1 … ⌃9.
`connectAtLaunch` („Beim Start verbinden“ im Editor, fehlt = `true`): Die
Instanz wird beim App-Start im Hintergrund verbunden, siehe
[Instanzwechsel](#instanzwechsel).

### SSH-Tunnel

- Tunnel: ein SSH-Master im Hintergrund, `/usr/bin/ssh -F <sshConfig>
  -o BatchMode=yes -o ConnectTimeout=15 -o ExitOnForwardFailure=yes
  -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ControlMaster=yes
  -o ControlPath=~/Library/Caches/dsh-codex/ssh/<id>.sock -o ControlPersist=10m
  -E ~/Library/Logs/dsh-codex/ssh-<id>.log -N -L 127.0.0.1:<lokal>:127.0.0.1:<remote> <host>`.
  Mit `ControlPersist` geht `ssh` nach Anmeldung und Weiterleitung selbst in den
  Hintergrund; die App merkt sich die PID aus `ssh -O check` und prüft sie alle
  5 s. Host-Schlüssel werden genau wie in der SSH-Konfiguration geprüft (dort
  `StrictHostKeyChecking yes`); die App schaltet das nie ab und fragt nie
  interaktiv (`BatchMode`).
- Wiederverwendung: Läuft beim Start schon ein Master auf dem Socket (z. B. vom
  letzten App-Lauf), wird er übernommen, eine fehlende Weiterleitung per
  `ssh -O forward` neu angefordert. Beim Beenden der App bleibt der Master
  stehen und endet von selbst 10 min nach der letzten Verbindung über ihn; ein
  Neustart der App ist so ohne neuen SSH-Handshake verbunden. „Neu verbinden“,
  Bearbeiten und Entfernen einer Instanz schließen ihn (`ssh -O exit`).
- Token: `ssh … -o ControlPath=<socket> <host> <tokenCommand>` über den Master.
  Die Ausgabe darf der Startlink (`…?token=…`, letzte passende Zeile zählt) oder
  das nackte Token sein. Ausgabe und Token werden weder protokolliert noch
  angezeigt. Das Token bleibt im Speicher und wird bei jedem Neuaufbau erst
  gegen `GET /api/info` geprüft; nur bei `401` läuft der Token-Befehl erneut.
  Schneller geht es, wenn der Dienst auf dem VPS mit `--ready-file <pfad>`
  startet: Dann reicht als Token-Befehl `cat <pfad>` statt `journalctl`
  (noch nicht so eingerichtet).
- Lauscht auf dem lokalen Port bereits ein dsh-codex (z. B. vom Starter
  `open-deepseek-vps-02.command`) und es gibt keinen eigenen Master, wird dieser
  Tunnel mitbenutzt; fällt er weg, baut die App einen eigenen auf. Ein fremder
  Dienst auf dem Port ergibt eine Fehlermeldung.
- Abbruch: Endet der Master, baut die App ihn sofort neu auf. Schlägt der
  Aufbau fehl, zeigt sie den Grund (letzte Zeile aus dem ssh-Protokoll) und
  versucht es mit Backoff (2, 4, 8 … max. 30 s) erneut. Alle 15 s prüft sie
  `GET /api/info`: `401` → Server wurde neu gestartet, Token neu holen und Seite
  neu laden; keine Antwort → Fehler, beim nächsten Mal Master neu aufbauen.
- `ssh`-Meldungen (ohne Token) stehen in `~/Library/Logs/dsh-codex/ssh-<id>.log`.

## Instanzwechsel

Ziel: nie ein leerer oder grauer Zwischenzustand.

- **Vorwärmen:** Beim Start verbindet die App zuerst die gezeigte Instanz, 2 s
  später alle anderen mit `connectAtLaunch` (Tunnel, Token und Seite). Jede
  verbundene Instanz hat ein eigenes `WKWebView`, das unsichtbar (Deckkraft 0,
  aber gerendert) im Fenster liegt. Die Web-UI kann beim Überfahren eines
  Eintrags im Wechsler `prewarm(id)` aufrufen.
- **Bereit:** Eine Seite gilt als bereit, wenn die Web-UI nach dem ersten
  vollständigen Render `dshxDesktop.ready()` meldet. Ältere UIs ohne diesen
  Aufruf (sie melden auch kein `setAppearance`) gelten 0,8 s nach `didFinish`
  als bereit, neuere spätestens nach 6 s.
- **Wechsel:** Ist die Zielseite bereit, blendet das Fenster in 150 ms über
  (bei „Bewegung reduzieren“ sofort). Sonst bleibt die bisherige Seite stehen;
  die 2 pt hohe Fortschrittslinie unter der Titelleiste läuft, und der Wechsler
  der Web-UI zeigt einen Spinner neben dem Zielnamen. War die Zielinstanz schon
  einmal sichtbar, erscheint sofort ihr letzter Schnappschuss
  (`WKWebView.takeSnapshot`, aufgenommen beim Verlassen oder vor dem Neuladen),
  leicht abgeblendet, mit der Pille „Verbinde mit VPS 2 …“.
- **Neu laden** (neues Token, ⌘R, abgestürzter Web-Prozess): Schnappschuss statt
  weißer Seite, dann Überblendung.
- **Hintergrund:** Fenster und Seitenfläche haben die Farbe, die die Seite per
  `setAppearance` meldet (`#FFFFFF`/`#181818` in den Standardthemen); die App
  merkt sie sich je Instanz für den nächsten Start und passt das Fenster an
  helles oder dunkles Theme an.
- **Fehler:** Über Schnappschuss oder eigener Seite erscheint oben ein Banner mit
  Grund, „Erneut versuchen“ und „Instanzen bearbeiten …“. Das vollflächige
  Statusbild gibt es nur noch, wenn eine Instanz noch nie sichtbar war und der
  Aufbau fehlschlägt.

## JavaScript-Brücke `window.dshxDesktop`

Jede Instanzseite bekommt per `WKUserScript` (Dokumentbeginn, nur Hauptframe)
ein eingefrorenes Objekt `window.dshxDesktop`. In einem normalen Browser fehlt
es – die Web-UI muss also immer `if (window.dshxDesktop)` prüfen. Die Brücke
spricht `window.webkit.messageHandlers.dshx` an und akzeptiert nur Aufrufe aus
dem Hauptframe mit genau dem Origin der eigenen Instanz.

### Eigenschaften (immer aktuell, Getter)

| Name | Typ | Bedeutung |
|---|---|---|
| `version` | `string` | Version der App (`CFBundleShortVersionString`). |
| `platform` | `"macos"` | |
| `current` | `string` | ID der Instanz, **zu der diese Seite gehört**. Solange die Seite sichtbar ist, ist das auch die aktive Instanz. |
| `instances` | `Array<{id, name, kind, active, status, unread, connectedAt?}>` | Alle Instanzen in Menüreihenfolge (Kopie). `kind`: `"local"`, `"ssh"`, `"url"`. `active`: wird gerade im Fenster gezeigt oder ist das Ziel eines laufenden Wechsels. `status`: `"idle"` (nicht gestartet), `"starting"`, `"ready"`, `"error"`. `unread`: zuletzt per `setBadge` gemeldete Zahl dieser Instanz. `connectedAt`: Zeitpunkt (ms) der letzten erfolgreichen Verbindung. |
| `capabilities` | `{revealInFinder, openInEditor, pickFolder: boolean}` | Nur bei lokalen Instanzen `true` (Pfade entfernter Server gibt es auf dem Mac nicht). |
| `titlebar` | `{height, trafficLightsWidth, fullScreen}` | Fensterrahmen in CSS-Pixeln, siehe unten. |

### Methoden (alle geben ein `Promise` zurück)

| Aufruf | Ergebnis | Wirkung |
|---|---|---|
| `switchTo(id)` | `boolean` | Zeigt Instanz `id` (startet/verbindet sie bei Bedarf). `false`, wenn es sie nicht gibt. Seiten anderer Instanzen bleiben im Hintergrund geladen, der Wechsel ist sofort. |
| `addInstance()` | `undefined` | Öffnet den nativen Dialog „Instanz hinzufügen“. Eine neu angelegte Instanz wird sofort angezeigt. |
| `editInstances()` | `undefined` | Öffnet das Fenster „Instanzen“. |
| `notify(title, body, options?)` | `true` | macOS-Mitteilung. Untertitel ist der Instanzname. `options.tag` (optional) ersetzt eine frühere Mitteilung mit demselben Tag und kommt beim Klick zurück (siehe Ereignisse). Beim ersten Aufruf fragt macOS nach der Erlaubnis. Die App unterdrückt nichts; die UI sollte nur melden, wenn `document.hidden` oder das Fenster nicht im Fokus ist. |
| `setBadge(n)` | `undefined` | Zahl auf dem Dock-Symbol für diese Instanz; die App zeigt die Summe aller Instanzen. `0` entfernt den Beitrag. |
| `revealInFinder(path)` | `boolean` | Zeigt die Datei im Finder. Nur lokale Instanzen, nur absolute existierende Pfade (`/…`, `~/…`, `file://…`). |
| `openInEditor(path)` | `boolean` | Öffnet Datei oder Ordner im Editor aus `instances.json`. Gleiche Einschränkungen; Programme werden nie direkt ausgeführt. |
| `pickFolder({title, prompt, defaultPath})` | `string \| null` | Ordnerauswahl (`NSOpenPanel`) als Sheet: nur Ordner, „Neuer Ordner“ möglich, eine Auswahl, Knopf `prompt` (Vorgabe „Projekt öffnen“), `title` als Hinweis. Startet in `defaultPath`, sonst im Ordner der letzten Auswahl, sonst `~`. `null` bei Abbruch und bei entfernten Instanzen. |
| `ready()` | `undefined` | Die Seite hat zum ersten Mal vollständig gerendert (Sidebar und Chat); erst dann blendet das Fenster zu ihr über. Einmal je Seitenladen. |
| `setAppearance({background, theme})` | `undefined` | Hintergrundfarbe (`#rrggbb`) und `"light"`/`"dark"`. Wer das meldet, ruft auch `ready()` auf. |
| `prewarm(id)` | `undefined` | Verbindet Instanz `id` im Hintergrund, falls sie noch nicht läuft (Hover im Wechsler). |

### Ereignisse (auf `window`)

| Ereignis | `detail` | Wann |
|---|---|---|
| `dshx-desktop-change` | `{current, instances}` | Liste, Namen, Status oder aktive Instanz haben sich geändert (auch Vollbild/Titelleiste). |
| `dshx-desktop-command` | `{command}` | Menübefehl der App: `"new-chat"` (⌘N), `"settings"` (⌘,), `"open-project"` (⌘O). Das Ereignis ist `cancelable`: Ruft die UI `event.preventDefault()` auf, gilt der Befehl als erledigt. Sonst klickt die App als Rückfall das erste Element aus `[data-dshx-command="<befehl>"]`, dann `.sb-nav .nav-item` bzw. `.topbar-new` (neuer Chat) oder `.sb-foot .nav-item` (Einstellungen). |
| `dshx-desktop-drag` | `{active}` | Ein Ordner aus dem Finder wird über eine lokale Instanzseite gezogen (`true`) oder verlässt sie (`false`). |
| `dshx-desktop-drop` | `{paths}` | Ordner (nur Ordner, keine Pakete) wurden auf einer lokalen Instanzseite abgelegt; die UI fügt sie als Projekte hinzu. WKWebView liefert der Seite selbst keine Pfade, darum nimmt die App den Drop an. |
| `dshx-desktop-notification` | `{tag}` | Der Nutzer hat eine Mitteilung dieser Instanz angeklickt; die App hat Fenster und Instanz bereits nach vorn geholt. |

### Fensterrahmen, CSS und Ziehen

- Das Fenster hat eine transparente Titelleiste; der Inhalt reicht bis oben.
  Die Ampel sitzt vertikal mittig in den obersten **48 px** (Höhe der
  `sb-head`/`topbar`-Zeile), 18 px vom linken Rand.
- Auf `<html>` setzt die Brücke die Klasse `dshx-desktop` (im Vollbild
  zusätzlich `dshx-fullscreen`) und die Variablen
  `--dshx-titlebar-height` (48px) und `--dshx-traffic-lights-width`
  (≈ 84px, im Vollbild `0px`): Breite, die links in der obersten Zeile frei
  bleiben muss.
- Übergangsweise fügt die Brücke `<style id="dshx-desktop-defaults">` ein, das
  `.sb-head` und `.topbar.is-collapsed` links um diese Breite einrückt. Die UI
  kann es entfernen (`document.getElementById('dshx-desktop-defaults')?.remove()`),
  sobald sie die Variablen selbst nutzt.
- **Fenster ziehen:** Ein Mausklick in den obersten 48 px oder auf ein Element
  mit `data-dshx-drag` verschiebt das Fenster, außer er trifft ein
  Bedienelement (`a, button, input, textarea, select, label, summary, [role=button]`,
  `[contenteditable]`, `[draggable=true]` …) oder etwas mit `data-dshx-no-drag`.
  Doppelklick dort zoomt bzw. minimiert gemäß Systemeinstellung.

### Instanzschalter oben links (Vorschlag für die Web-UI)

```js
// Name oben links in der Sidebar = aktuelle Instanz; Klick öffnet die Liste.
function instanceMenu() {
  const d = window.dshxDesktop;
  if (!d) return null;                       // Browser: kein Schalter
  return {
    current: d.instances.find((i) => i.id === d.current),
    items: d.instances,                       // {id, name, kind, active, status}
    pick: (id) => d.switchTo(id),
    add: () => d.addInstance(),
    manage: () => d.editInstances(),
  };
}
window.addEventListener('dshx-desktop-change', () => rerender());
window.addEventListener('dshx-desktop-command', (e) => {
  if (e.detail.command === 'new-chat') { newChat(); e.preventDefault(); }
  if (e.detail.command === 'settings') { openSettings(); e.preventDefault(); }
});
```

Statuspunkte: `ready` grün, `starting` grau/animiert, `error` rot, `idle`
ohne Punkt. ⌃1 … ⌃9 schalten bereits nativ um. Die Web-UI setzt das in
`ui/src/components/instances.js` um (dazu Spinner beim Wechsel, Zahl ungelesener
Chats, `prewarm` beim Überfahren, `ready` und `setAppearance`).

## Menüs und Tastenkürzel

| Menü | Eintrag | Kürzel |
|---|---|---|
| dsh-codex | Einstellungen … (Befehl `settings` an die Seite) | ⌘, |
| dsh-codex / Instanz | Instanzen bearbeiten … | ⌥⌘, |
| Ablage | Neuer Chat (Befehl `new-chat`) | ⌘N |
| Ablage | Projekt öffnen … (Befehl `open-project`) | ⌘O |
| Ablage | Fenster schließen (App läuft weiter, Dock-Symbol öffnet wieder) | ⌘W |
| Bearbeiten | Widerrufen, Ausschneiden, Kopieren, Einsetzen, Alles auswählen, Suchen | Standard |
| Darstellung | Neu laden / Originalgröße / Vergrößern / Verkleinern / Vollbild | ⌘R / ⌘0 / ⌘+ / ⌘- / ⌃⌘F |
| Instanz | eine Zeile je Instanz (Häkchen = aktiv, Plakette bei Fehler/Verbinden) | ⌃1 … ⌃9 (⌘1 … ⌘9 bleiben für Chats frei, wie in Codex) |
| Instanz | Neu verbinden | ⇧⌘R |
| Instanz | Instanz hinzufügen … | |
| Hilfe | Protokolle anzeigen, instances.json im Finder zeigen, Computer-Use-Helfer installieren …, Mitteilungen testen | |

Weiteres Verhalten: Fensterposition und -größe werden gemerkt; Links zu
anderen Origins (und `target="_blank"`) öffnen im Standardbrowser;
`alert`/`confirm`/`prompt` und Dateiauswahl erscheinen als native Dialoge;
der Web-Inspektor von Safari ist für die Seiten freigeschaltet
(`isInspectable`).

## Dateien

| Pfad | Inhalt |
|---|---|
| `~/Library/Application Support/dsh-codex/instances.json` | Instanzen (0600) |
| `~/Library/Application Support/dsh-codex/run/` | Ready- und PID-Dateien lokaler Server (0700) |
| `~/Library/Caches/dsh-codex/ssh/` | ControlMaster-Sockets |
| `~/Library/Logs/dsh-codex/desktop.log` | Ablauf der App (Status, Laden, Beenden) |
| `~/Library/Logs/dsh-codex/local-<id>.log`, `ssh-<id>.log` | Server- bzw. ssh-Ausgabe, Tokens maskiert |
| Schlüsselbund, Dienst `de.yousufzai.dsh-codex.instance` | Tokens von `url`-Instanzen |
| Benutzereinstellungen der App (`background.<id>`, `theme.<id>`, `lastProjectParent`) | Seitenfarbe und Theme je Instanz, Startordner der Ordnerauswahl |

`DSHX_DESKTOP_SUPPORT_DIR=<ordner>` legt Instanzen, `run/`, `logs/` und `ssh/`
(Sockets; Pfad kurz halten, max. 104 Byte) in diesen Ordner, und der
Computer-Use-Helfer wird nicht nach `~/Applications` kopiert. So läuft eine
zweite Kopie (z. B. ein Testbuild) neben der installierten App, ohne deren
Daten anzufassen. Benutzereinstellungen und Website-Daten hängen an der
Bundle-ID; eine Testkopie bekommt darum besser eine eigene.

## Bekannte Grenzen

- Keine Notarisierung, keine Developer-ID; nur arm64.
- Die lokale Instanz bekommt bei jedem Start einen neuen Port (`--port 0`).
  `localStorage` hängt am Origin, daher überleben dort gespeicherte
  UI-Einstellungen (Theme, eingeklappte Sidebar) einen App-Neustart nicht.
  Abhilfe später: Einstellungen serverseitig speichern oder einen festen Port
  wählen.
- Das Ziehen des Fensters über die Brücke braucht eine gehaltene Maustaste;
  automatisiert getestet ist nur der Doppelklick (Zoomen).
