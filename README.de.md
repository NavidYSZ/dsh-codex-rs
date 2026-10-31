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

HuntProxy muss in **Aether → Plugins → MCP-Server** eingerichtet sein, auch
wenn es bereits mit einem anderen Agenten verbunden ist. „Immer laden“
aktivieren oder im Chat `@huntproxy` nennen. PTC verwendet die MCP-Werkzeuge
direkt. [Einrichtung und Fehlerdiagnose](docs/browser-use.md).

Die Kontext-Einstellung greift auch zwischen abgeschlossenen PTC-/Goal-
Arbeitsrunden. „Jede Runde“ komprimiert ältere Inhalte ab 1.024 neuen
komprimierbaren Tokens; Standard ohne Goal behält die Grenze von 8.000 Tokens
am Turn-Ende. Das jüngste zusammengehörige Tool-Ergebnis, wartende Nachrichten
und der kanonische Goal-Zustand bleiben erhalten. Stop verhindert weitere
automatische Goal-Runden, auch während einer Zusammenfassung.

## Unterschied zum alten Repository

[NavidYSZ/dsh-codex](https://github.com/NavidYSZ/dsh-codex) enthält das ältere
**Plugin für eine vorhandene DeepSeek-Harness-App**. Dessen Installationsschritte
bauen keine zweite App. Für die eigenständige App verwende dieses Repository
und seine Releases.

Quellcode, CLI-/Web-Installation, weitere Funktionen und Einschränkungen stehen
in der [englischen README](README.md) und den [Desktop-Buildhinweisen](desktop/README.md).
