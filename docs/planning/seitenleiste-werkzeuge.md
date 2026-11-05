# Rechte Seitenleiste mit Browser und Dateien statt Diff

- **Status:** Umgesetzt
- **Stand:** 2026-11-05 18:37 (Europe/Berlin)
- **Commits:** `b54484c`

## Ziel

Beim Öffnen der rechten Seitenleiste erscheint heute immer zuerst die
Diff-Ansicht („Änderungen“). Der Nutzer braucht sie dort nicht und will die
früheren Schalter zurück: **Browser** und **Dateien**. Die Leiste soll
wieder mit diesen Werkzeugen arbeiten, nicht mit dem Diff.

## Ausgangslage

- `ui/src/components/panel.js`, `Panel`: feste Reiter `changes` (Diff,
  `t.changes` mit Zähler) und `files` (`t.filesTab`), danach die Zusatzreiter
  aus `ui/src/components/app.js` (`live` aus `useLiveTab()`, `team` aus
  `useTeamTab()`) und die offenen Dateireiter `file:<rel>`. Unbekannte Reiter
  fallen auf die Diff-Ansicht zurück.
- `ui/src/components/live.js`, `useLiveTab()`: liefert den Browser-Reiter
  nur, solange `store.live` für diesen Chat existiert; Beschriftung
  Browser/Computer/App je nach `live.tool`. `LivePanel` zeigt Bild, Caption
  und Stopp. Der Server (`crates/dsh-codex/src/live.rs`, `LiveHub::latest`)
  schickt beim Verbinden das letzte Live-Bild des Chats nach.
- `ui/src/state.js`: Startwert `panel.tab = 'changes'`; `freshPanel` (Chat-
  Wechsel) setzt Dateireiter auf `changes` zurück; `closeFile` fällt auf
  `changes` zurück; `onLive` wechselt bei einem neuen Live-Aufruf von
  `changes`/`files` auf `live`; `openPanel(tab)`, `togglePanel` (Kopfzeilen-
  knopf, pinnt seit `4e4b442`), `closePanel`, `setPanelTab`.
- Aufrufer von `openPanel('changes')`: `ChangesCard` in
  `ui/src/components/transcript.js` („Ansehen“ und „+N“). `openPanel('live')`:
  „Im Panel zeigen“ im schwebenden Bild (`LiveFloat`). `openFile`: Dateilinks
  im Verlauf, Dateibaum, Diff-Ansicht.

## Plan

1. **Reiterleiste:** Browser (`live`, immer vorhanden) – Dateien (`files`) –
   Team (falls vorhanden) – temporärer Diff-Reiter (nur auf ausdrücklichen
   Wunsch) – Dateireiter. Der feste Diff-Reiter entfällt.
2. **Browser ohne Live-Arbeit:** ruhiger Leerzustand mit Browser-Symbol und
   Hinweis (DE/EN), dass die Ansicht hier erscheint, sobald der Agent den
   Browser oder den Computer nutzt. Gibt es ein letztes Bild dieses Chats,
   zeigt der Reiter es wie bisher. Computer/App behalten ihre Beschriftung im
   selben Reiter.
3. **Startreiter** (`homeTab` in `state.js`), angewendet beim Öffnen über den
   Kopfzeilenknopf, `openPanel()` ohne Reiter, beim Chat-Wechsel und als
   Rückfall:
   - arbeitet der Agent in diesem Chat gerade im Browser/Computer (aktiv oder
     letzter Schritt jünger als die Nachlaufzeit des schwebenden Bildes)
     → Browser;
   - sonst die zuletzt vom Nutzer gewählte Schaltfläche Browser/Dateien;
   - ohne gespeicherte Wahl → Browser, wenn der Chat ein Live-Bild hat, sonst
     Dateien.
4. **Gemerkte Wahl:** neuer localStorage-Schlüssel `dshx-panel-home`, Werte
   nur `live` oder `files`. Gespeichert wird nur ein Klick des Nutzers auf
   den Browser- oder Dateien-Reiter, nicht automatische Wechsel (`onLive`)
   oder „Im Panel zeigen“. `changes` und Dateireiter werden nie als Start
   gespeichert oder wiederhergestellt.
5. **Diff nur auf Wunsch:** `openPanel('changes')` blendet einen temporären,
   schließbaren Reiter „Änderungen“ (mit Zähler) ein und merkt sich den
   vorigen Reiter (`panel.back`). Schließen kehrt dorthin zurück (oder zum
   Startreiter, wenn der vorige Reiter nicht mehr existiert). Schließen der
   ganzen Leiste und Chat-Wechsel entfernen den temporären Reiter.
6. **Live-Wechsel:** `onLive` wechselt bei einem neuen Aufruf weiter von
   Dateien auf Browser, aber nicht mehr aus dem ausdrücklich geöffneten
   Diff; dort erscheint wie bisher das schwebende Bild.
7. **Unverändert:** Pinnen über den Kopfzeilenknopf (`4e4b442`),
   `LiveFloat`-Regeln inkl. `native`, Dateivorschau, Breitenziehen, Team-
   Reiter. i18n in beiden Sprachen, Stil in `ui/styles/app.css`, ein
   Eintrag in `CHANGELOG.md` unter „Unreleased“.

Bewusst ausgelassen: keine Änderungen am Rust-Server, keine neuen Tests,
kein Build/Install der Mac-App, keine Browser-Automatisierung.

## Umsetzung

Commit `b54484c` auf `codex-app` (nicht gepusht).

- `ui/src/state.js`
  - Neu: `PANEL_HOME_KEY = 'dshx-panel-home'` und `panelChoice()` (liest nur
    `live` oder `files`), `LIVE_LINGER_MS = 8000` (vorher als `LINGER_MS` in
    `live.js`), `liveNow(live)` (aktiv oder letzter Schritt jünger als
    `LIVE_LINGER_MS`, Browser nicht beendet) und `panelHome(live)` mit der
    Reihenfolge aus Plan Schritt 3.
  - Panel-Zustand erweitert um `diff` (temporärer Diff-Reiter sichtbar) und
    `back` (Rückkehrziel). Startwert `tab: panelHome(null)` statt `changes`.
  - `freshPanel` (Chat-Wechsel, neuer Chat): Dateireiter und Diff weg, Reiter
    = `panelHome(null)`, Pinnen unverändert.
  - `openPanel('changes')`: setzt `diff: true`, `back` = vorher offener Reiter
    (bei geschlossener Leiste der Startreiter). `openPanel()` ohne Reiter
    bleibt beim offenen Reiter bzw. nimmt den Startreiter.
  - `togglePanel` (Kopfzeilenknopf): pinnt wie seit `4e4b442`, öffnet aber
    immer auf `panelHome(live)` und verwirft den Diff-Reiter. `closePanel`
    verwirft den Diff-Reiter ebenfalls.
  - `setPanelTab(tab)`: speichert nur `live`/`files` als gemerkte Wahl.
  - Neu `closeDiff()`: schließt den Diff-Reiter; war er aktiv, zurück zu
    `back`, wenn dieser Reiter noch existiert (Browser, Dateien, Team mit
    `store.team`, offener Dateireiter), sonst zum Startreiter.
  - `closeFile`: letzter Dateireiter fällt auf den Startreiter statt auf
    `changes` zurück.
  - `onLive`: wechselt von Dateien auf Browser bei einem neuen Aufruf (wie
    bisher) und zusätzlich beim ersten Bild des Chats, solange keine Wahl
    gespeichert ist (deckt das vom Server nachgereichte letzte Bild nach dem
    Chat-Wechsel ab). Aus dem Diff wird nicht mehr weggewechselt. Der Wechsel
    nutzt `setPanel` direkt und wird nicht als Wahl gespeichert.
- `ui/src/components/live.js`: `useLiveTab()` liefert den Reiter immer
  (Beschriftung Browser/Computer/App wie bisher, aktiver Punkt bei laufender
  Arbeit). `LivePanel` zeigt ohne Live-Daten Globus-Symbol und
  `t.browserIdle`, mit Daten wie bisher Caption, Stopp und das letzte Bild.
  `LiveFloat` unverändert bis auf die gemeinsame Konstante.
- `ui/src/components/app.js`: übergibt `liveTab` getrennt an `Panel`;
  `extraTabs` enthält nur noch Team.
- `ui/src/components/panel.js`, `Panel`: Reiterfolge Browser – Dateien –
  Team – Diff (nur bei `panel.diff`, schließbar, mit Zähler) – Dateireiter.
  Ein nicht mehr vorhandener Reiter (z. B. Team beendet) zeigt den
  Startreiter statt der Diff-Ansicht. Dateireiter und Diff-Reiter teilen die
  Hilfsfunktion `closable`.
- `ui/src/i18n.js`: `browserIdle` (EN/DE).
- `ui/styles/app.css`: Abstand für den Zähler im Diff-Reiter
  (`.ptab-name .ptab-count`).
- `CHANGELOG.md`, „Unreleased“: ein Eintrag.

Abweichungen vom Plan: keine inhaltlichen. Ergänzt wurde nur der
`onLive`-Fall „erstes Bild ohne gespeicherte Wahl“, weil das letzte Bild
eines Chats erst nach dem Chat-Wechsel über den Stream ankommt.

## Prüfung

- `node --check` für `ui/src/state.js`, `ui/src/components/panel.js`,
  `ui/src/components/live.js`, `ui/src/components/app.js`, `ui/src/i18n.js`:
  fehlerfrei.
- Alle Aufrufer geprüft (`grep`): `openPanel('changes')` in
  `ui/src/components/transcript.js` („Ansehen“, „+N“) öffnet jetzt den
  temporären Diff-Reiter; `openPanel('live')` in `LiveFloat`; `openFile` aus
  Verlauf, Dateibaum und Diff; `togglePanel` in der Kopfzeile. In `ui/dev`,
  `desktop` und der Rust-Seite gibt es keine Abhängigkeit vom alten Reiter.
- Kein `ui/dev/*_check.mjs` betrifft Seitenleiste oder Panel-Zustand, daher
  wurde keiner ausgeführt.
- Nicht geprüft (auf Wunsch des Nutzers): keine Tests, Lints, Browser-
  Automatisierung oder Sichtprüfung in der App; kein Build/Install der
  Mac-App.

## Offen

- Sichtprüfung in der laufenden App: Leerzustand des Browser-Reiters,
  Diff-Reiter mit Zähler und Schließen, gemerkte Wahl nach Neustart.
- `main` und GitHub sind nicht aktualisiert; der Commit liegt nur auf
  `codex-app`.
- Bleibt die Leiste wegen eines verschwundenen Reiters (z. B. Team beendet)
  auf dem Browser als Rückfall stehen, blendet `LiveFloat` das schwebende
  Bild trotzdem ein, weil es nur `panel.tab` prüft. Seltener Randfall.
