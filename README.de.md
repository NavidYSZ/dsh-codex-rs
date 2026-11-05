# Aether (dsh-codex)

[English and full documentation](README.md)

**Aether ist eine eigenständige App.** Sie wird neben DeepSeek Harness
installiert und hat eigene Chats, Einstellungen und Zugangsdaten unter
`~/.dsh-codex`. Das Repository heißt `dsh-codex-rs`, die CLI weiterhin `dsh-codex`.

## Installation auf dem Mac

1. Unter [Releases](https://github.com/NavidYSZ/dsh-codex-rs/releases) die
   macOS-ARM64-DMG herunterladen.
2. DMG öffnen und **Aether.app** nach **Programme** ziehen.
3. Aether starten und unter Einstellungen den gewünschten Modellanbieter
   einrichten. Agent, PTC-Laufzeit und Computer-Use-Helfer sind enthalten.

Voraussetzungen: Apple Silicon (M1 oder neuer), macOS 14 oder neuer.
Die Vorschau ist ad hoc signiert, noch nicht notarisiert. Falls macOS den
Start blockiert, nach Prüfung der Downloadquelle die Freigabe für diese App
unter **Systemeinstellungen → Datenschutz & Sicherheit → Dennoch öffnen**
verwenden. Die systemweite Sicherheitsfunktion muss nicht abgeschaltet werden.

Für Computer Use optional dem Helfer **DSH Computer Use** Bedienungshilfen
und Bildschirmaufnahme erlauben.

Du hast bereits unsere frühere `dsh-codex.app`? Beende sie vor dem Start von
Aether. Die vorhandenen Daten und gespeicherten Instanzen bleiben unter den
bisherigen Pfaden erhalten. Nur eine Kopie unserer eigenständigen App
installiert lassen. Das originale DeepSeek Harness ist davon unabhängig.

## PTC, Steering und Warteschlange

- **PTC:** Beim Anlegen eines Chats den Modus PTC auswählen. Der Agent
  koordiniert Tools über TypeScript (`run_code`), kann unabhängige Aufrufe
  bündeln und große Ergebnisse vor der Rückgabe filtern. Berechtigungen,
  Freigaben und Abbruch gelten weiterhin. Standard und Minimal bleiben verfügbar.
- **Steering:** Während einer Aufgabe fügt Enter die Nachricht am nächsten
  Verarbeitungsschritt ein. Sie bleibt bis dahin sichtbar.
- **Warteschlange:** ⌘Enter stellt die Nachricht für einen eigenen nächsten
  Durchlauf an. In den Einstellungen lassen sich die beiden Tastenbelegungen
  vertauschen. Fehlgeschlagene Sendungen behalten Text und Anhänge; ein
  Wiederholungsversuch derselben Sendung wird nicht doppelt ausgeführt.

## Browser Use, HuntProxy und Kontext

Unter **Einstellungen → Browser Use** lassen sich Browserpfad, Aktivierung und
aktiver Status prüfen. **Browser testen** startet ein separates Testprofil und
prüft Öffnen, Tippen, Klicken und Lesen auf einer lokalen Seite. Eigene Tabs und
Anmeldungen bleiben erhalten. Änderungen an Pfad/Aktivierung erfordern einen
Neustart der betroffenen Instanz; die Oberfläche zeigt das ausdrücklich an.

**Dein eigenes Chrome.** Sag „nutze mein Chrome“, „das offene Chrome“ oder
nenne einen offenen Tab: Der Agent verbindet sich dann mit deinem laufenden
Google Chrome (auch Chromium, Brave, Edge) samt Tabs und Anmeldungen, statt
einen zweiten Browser zu starten (`browser: "user"`). Einmalig in Chrome unter
`chrome://inspect/#remote-debugging` → „Allow remote debugging for this
browser instance“ erlauben; fragt Chrome beim Verbinden „Allow remote
debugging?“, auf „Allow“ klicken. Aether hält diese Verbindung, solange es
läuft, sodass Chrome nicht bei jeder Aktion erneut fragt. Der Agent beginnt
auf dem Tab, den du gerade siehst, öffnet für neue Seiten einen eigenen Tab,
navigiert deinen Tab nie weg und schließt deinen Browser nie. Einstellungen →
Browser Use zeigt, ob die Verbindung gerade möglich ist.

HuntProxy muss in **Aether → Plugins → MCP-Server** eingerichtet sein, auch
wenn es bereits mit einem anderen Agenten verbunden ist. Im Eingabefeld unter
**Konnektoren** für den Chat einschalten, im Chat `@huntproxy` nennen oder auf
der Karte „In neuen Chats vorausgewählt“ aktivieren. PTC verwendet die
MCP-Werkzeuge direkt. [Einrichtung und Fehlerdiagnose](docs/browser-use.md).

MCP-Server kommen nur in einen Chat, wenn der Chat sie auswählt.
**Konnektoren** im Eingabefeld zeigt jeden aktivierten Server mit Status und
Anzahl der Tools; der Schalter nimmt ihn in diesen Chat auf oder heraus (auf
der Startseite: für den Chat, den die erste Nachricht anlegt). `@Name` in
einer Nachricht schaltet einen Server ebenfalls ein. Server mit `always =
true` („In neuen Chats vorausgewählt“, Markierung *Standard*) starten in jedem
neuen Chat; jeder Chat kann sie trotzdem ausschalten und bekommt dann weder
ihre Tools noch ihre Anweisungen. Ein Chat behält die Vorauswahl, mit der er
angelegt wurde; eine spätere Änderung lässt bestehende Chats unberührt. Nur
Chats von vor dieser Aufzeichnung, die nie selbst gewählt haben, folgen ihr
weiter. Der Minimal-Modus hat keine MCP-Server.
Subagenten und PTC-Programme sehen nur die Server ihres Chats.

Eine Runde umfasst die vollständige Antwort des Agenten samt allen
Tool-Aufrufen und PTC-Programmen. **„Nach jeder Antwort“** komprimiert erst
danach, bevor die nächste Goal-Runde oder Folgefrage beginnt. Einzelne
Tool-Aufrufe lösen diese Komprimierung nicht aus. Für PTC/aktive Goals sind
mindestens 1.024 neue komprimierbare Tokens nötig, für Standard ohne Goal
8.000. Die letzte Antwort, wartende Nachrichten und der kanonische Goal-Zustand
bleiben erhalten. Subagenten und einmalige CLI-Aufgaben überspringen den
abschließenden Checkpoint. Der Schutz bei vollem Kontext bleibt verfügbar.
Stop verhindert weitere automatische Goal-Runden, auch während einer
Zusammenfassung.

## Aether auf dem iPhone

Die iOS-App in `ios/` verbindet sich mit jedem Rechner, auf dem `dsh-codex
web` läuft: mit diesem Mac, mit einem VPS, der nur SSH nach außen öffnet, oder
mit einem Server, der beim Accounts-Hub angemeldet ist. Sie zeigt dieselben
Chats, Tool-Zeilen, Freigaben und die Live-Ansicht wie die Web-Oberfläche.

Koppeln ist ein Scan:

1. **Web-UI → Einstellungen → Aether App:** Listener einschalten (TLS auf
   Port 7443, die App pinnt das Zertifikat) und „Gerät koppeln“. Die Seite
   zeigt einen QR-Code und einen `aether://pair#…`-Link.
2. **Terminal:** `dsh-codex pair` druckt QR-Code oder Link; `dsh-codex pair
   --list` und `dsh-codex pair --revoke <id>` verwalten die Geräte.
3. **VPS hinter SSH:** `dsh-codex pair --ssh <host> -F <ssh_config>` legt
   einen ed25519-Schlüssel für das Telefon an, trägt eine eingeschränkte
   `authorized_keys`-Zeile ein (nur Port-Weiterleitung auf den Loopback-Port
   des Servers) und druckt den QR-Code. Die App tunnelt dann über Port 22.

Ist auf dem Mac die Firewall aktiv, `dsh-codex` (oder die Aether-App) einmal
unter Systemeinstellungen → Netzwerk → Firewall erlauben; bis dahin antwortet
nur die Loopback-Adresse und das iPhone erreicht den Listener nicht.

Client-Tokens liegen gehasht in `$DSHX_HOME/mobile/clients.json`; ein Widerruf
trennt die Verbindungen des Geräts. Der Listener ist standardmäßig aus und
liefert weder Vorschauen noch den OAuth-Callback aus. Protokoll:
[docs/aether/SPEC.md](docs/aether/SPEC.md); Bauen der App:
[ios/README.md](ios/README.md). Der Accounts-Hub (`crates/dshx-hub`,
`aether-hub`: Registrierung, Sitzungen, Tiers, Relay) ist umgesetzt, aber
nicht online.

## Ordner zwischen Geräten übertragen

Die Mac-App kopiert, verschiebt oder synchronisiert einen Projektordner von
einer Instanz auf eine andere (dieser Mac, ein VPS hinter SSH, …): Ablage →
„Ordner übertragen …“ oder in der Web-UI das Projektmenü → „Auf anderes Gerät
übertragen …“. Quellordner, Zielinstanz, übergeordneten Zielordner und Art
wählen:

- **Kopieren** legt den Ordner am Ziel neu an; ein nicht leeres Ziel wird
  abgelehnt.
- **Verschieben** kopiert, prüft jede Datei am Ziel (Größe und Änderungszeit)
  und löscht erst dann den Quellordner.
- **Synchronisieren** überträgt neue und geänderte Dateien; auf Wunsch werden
  Dateien gelöscht, die es nur am Ziel gibt.

`node_modules`, `target`, `.venv`, `__pycache__`, `DerivedData` und ähnliche
Build-Ordner sind vorab ausgeschlossen; `.git` und versteckte Dateien werden
mitgenommen. „Vorschau“ zeigt vorher Anzahl und Größe. Die Instanzen sprechen
nie direkt miteinander: Die App streamt den Ordner als tar+zstd über eine
0600-Datei in ihrem Cache und trägt ihn am Ziel als Projekt ein. Jeder Server
nimmt nur Pfade im Benutzerordner und in seinen Projektordnern an, schreibt
nie durch einen Symlink und lehnt Archiveinträge mit `..` oder absoluten
Pfaden ab. API: [docs/aether/TRANSFER.md](docs/aether/TRANSFER.md).

## Unterschied zum alten Repository

[NavidYSZ/dsh-codex](https://github.com/NavidYSZ/dsh-codex) enthält das ältere
**Plugin für eine vorhandene DeepSeek-Harness-App**. Dessen Installationsschritte
bauen keine zweite App. Für die eigenständige App verwende dieses Repository
und seine Releases.

Quellcode, CLI-/Web-Installation, weitere Funktionen und Einschränkungen stehen
in der [englischen README](README.md) und den [Desktop-Buildhinweisen](desktop/README.md).
