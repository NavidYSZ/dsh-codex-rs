# Fehler seit der Umbenennung zu Aether

**Status:** In Arbeit
**Stand:** 2026-11-05, 19:20 Uhr
**Commits:** –

## Ziel

Der Nutzer: „Diese ganzen Bugs, die seit ein paar Tagen sind (seitdem wir auf
Aether gewechselt sind), müssen einmal alles aufgedröselt und gefixt werden.“

## Ausgangslage

Verglichen wurde der letzte Stand vor der Umbenennung (`74edaec`, Build 107)
mit `b162da2`. Drei Prüf-Subagenten haben am 2026-11-05 nur gelesen: UI und
Mac-App (`ui/`, `desktop/`), Rust-Server und Agent (`crates/`, ohne Hub) sowie
die echten Sitzungsprotokolle und App-Logs seit dem 03.11.

Schon vorher behoben: Subagenten-Zeilen aus PTC fehlten (`4e4b442`),
Seitenleiste öffnete in jedem Chat (`4e4b442`), Browser öffnete ein neues
Chrome-Fenster und zeigte zwei Vorschauen (`6d9aa81`), Chrome-Freigabe-Schleife
(`300253d`), Seitenleiste öffnete auf Diff (`b54484c`, siehe
[seitenleiste-werkzeuge.md](seitenleiste-werkzeuge.md)).

Ohne Codeänderung erledigt (2026-11-05, 19:15 Uhr): Ein verwaister zweiter
Server (`dsh-codex web --port 0 --ready-file /tmp/aether-ready.json`, seit
01:51 Uhr, Build-117-Code, gestartet von einer Installationsprüfung) hielt die
Sperre auf dem Bugbounty-Hauptchat und lief dort um 13:37 Uhr eine Goal-Runde
ohne Nutzernachricht. Er hatte keinen laufenden Chat und wurde mit SIGTERM
beendet.

## Plan

Zwei Behebungs-Subagenten arbeiten parallel in getrennten Worktrees mit
getrennten Dateien. Jeder schreibt seine Umsetzung in eine eigene Datei.

### Teil A – Server, Agent und Werkzeuge → [regressionen-server-und-agent.md](regressionen-server-und-agent.md)

| Nr. | Fehler | Ort | Beleg |
|---|---|---|---|
| A1 | Zwei Server teilen sich ein Datenverzeichnis; der zweite hält Chat-Sperren und arbeitet unbemerkt weiter | `crates/dsh-codex/src/web.rs`, `main.rs` | Log-Prüfung, verwaister Server |
| A2 | Beim Beenden laufende Jobs bekommen kein `job/settled`; nach dem Neustart „unknown job“ | `crates/dshx-agent/src/agent.rs` `dispose`, `jobs.rs` | 8 Jobs ohne Abschluss, 11-03 bis 11-05 |
| A3 | Lese-vor-Bearbeiten-Prüfung vergisst nach einem Neustart alle gelesenen Dateien | `crates/dshx-tools/src/state.rs` | 7 Fehler „file has not been read“ |
| A4 | `chmod` lässt Bearbeiten mit „file changed since it was read“ scheitern (ctime im Dateistand) | `crates/dshx-tools/src/fs/mod.rs` `probe`, `fs/mutate.rs` | 11-04 23:16 und 23:22 |
| A5 | Abgebrochener Modell-Stream (`STREAM_CLOSED`) wird nicht wiederholt; Turn endet mit Fehler, Goal bleibt stehen | `crates/dshx-llm/src/retry.rs` | 11-03 10:42, 11-05 02:33 |
| A6 | Komprimierung mit HY4 dauert bis 287 s und schreibt Zusammenfassungen bis 41k Token | `crates/dshx-agent/src/compaction.rs`, `config.rs` | 6 Läufe 11-04/05 |
| A7 | Ein noch offener Chrome-Dialog blockiert Browser-Aufrufe aller anderen Chats bis 60 s (`pause_cast` wartet auf die Sperre) | `crates/dshx-browser/src/tool.rs`, `browser.rs` | Code-Prüfung |
| A8 | Erstes Verbinden mit dem eigenen Chrome prüft bis 40 Tabs nacheinander, bei eingefrorenen Tabs bis über die Tool-Zeit | `crates/dshx-browser/src/browser.rs` `visible_tab` | Code-Prüfung |

### Teil B – Oberfläche und Mac-App → [regressionen-oberflaeche-und-mac.md](regressionen-oberflaeche-und-mac.md)

| Nr. | Fehler | Ort | Beleg |
|---|---|---|---|
| B1 | Beenden der App (Dock, Installation) bricht laufende Chats ohne Rückfrage ab | `desktop/Sources/AppDelegate.swift` `applicationShouldTerminate` | 11-03 15:10, 11-04 13:29 |
| B2 | „In neuen Chats vorausgewählt“ wirkt auch auf bestehende Chats ohne eigene Wahl | `ui/src/components/mcp.js`, `crates/dsh-codex/src/runtime/ondemand.rs` | Code-Prüfung |
| B3 | Kopplungs-QR-Code veraltet nach Ein-/Ausschalten oder Portwechsel und kann ohne Adresse sein | `ui/src/components/mobile.js` | Code-Prüfung |
| B4 | Konnektor-Auswahl der Startseite wird an einen neuen Minimal-Chat geschickt und schlägt fehl | `ui/src/components/composer-extras.js` `applyStagedMcp` | Code-Prüfung |
| B5 | Konnektor-Liste der Startseite wird nur einmal geladen und veraltet | `composer-extras.js` `loadStaged` | Code-Prüfung |
| B6 | Zufallsport 0 des Mobil-Zugangs lässt sich weder sehen noch speichern | `mobile.js`, `crates/dshx-server/src/mobile.rs` `view` | Code-Prüfung |
| B7 | Karte „Dein eigenes Chrome“ erscheint auch auf Linux-Servern | `ui/src/components/browser-settings.js` | Code-Prüfung |
| B8 | SSH-Kopplungsbefehl nennt `dsh-codex` ohne Pfad und ohne Benutzer/Home für root-Instanzen | `mobile.js` | Code-Prüfung |
| B9 | Umbenennen-Feld eines Geräts bekommt keinen Fokus | `mobile.js` | Code-Prüfung |
| B10 | Übertragen eines leeren Ordners legt am Ziel nichts an und scheitert | `desktop/Sources/TransferSheet.swift` `perform` | Code-Prüfung |

Bewusst ausgelassen: `grep`-Zeitüberschreitung ohne Teilergebnis (Einzelfall),
undatierte SSH-Meldungen „channel open failed“, Fehler von DeepSeek, HY4
(Tencent-Dienst gestoppt), HuntProxy, Webseiten und Modellfehler in `run_code`.

## Umsetzung

Siehe die beiden Teil-Dateien.

## Prüfung

Auf Nutzerwunsch keine Smoke-Tests, Testsuiten oder Lints; nur Kompilieren und
höchstens einzelne gezielte Prüfungen.

## Offen

- Zusammenführen beider Teile, Build, Installation, Push auf `main`.
- VPS 1, VPS 2 und VPS 5 laufen noch auf `c2c762c`.
