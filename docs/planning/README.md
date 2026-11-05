# Planung

Hier liegen geplante Funktionen und Korrekturen für Aether, eine Datei pro
Vorhaben. Subagenten schreiben den Plan, bevor sie bauen, und tragen nach dem
Bauen ein, wie es konkret umgesetzt wurde. So bleibt nachvollziehbar, was
geplant, was gebaut und was offen ist.

## Regeln für jede Plandatei

- Dateiname: kurz und sprechend, Kleinbuchstaben mit Bindestrichen, z. B.
  `seitenleiste-werkzeuge.md`.
- Sprache: Deutsch. Code, Pfade und Befehle bleiben, wie sie sind.
- Kopf mit drei Zeilen:
  - **Status:** `Geplant`, `In Arbeit`, `Umgesetzt` oder `Verworfen`
  - **Stand:** Datum und Uhrzeit (Europe/Berlin) der letzten Änderung
  - **Commits:** Kurz-Hashes, sobald etwas committet ist
- Abschnitte:
  1. **Ziel** – was der Nutzer will, in seinen Worten zusammengefasst.
  2. **Ausgangslage** – wie es heute funktioniert, mit Dateien und Funktionen.
  3. **Plan** – die Schritte, Entscheidungen und bewusst ausgelassene Punkte.
  4. **Umsetzung** – erst nach dem Bauen: was konkret geändert wurde (Dateien,
     Funktionen, Datenformate, UI-Verhalten), Abweichungen vom Plan und warum.
  5. **Prüfung** – was tatsächlich geprüft wurde und was nicht.
  6. **Offen** – Restpunkte und Folgeschritte.
- Keine Zugangsdaten, Tokens, persönlichen Daten oder Chat-Protokolle.

## Übersicht

| Datei | Vorhaben | Status |
|---|---|---|
| [seitenleiste-werkzeuge.md](seitenleiste-werkzeuge.md) | Rechte Seitenleiste mit Browser und Dateien statt Diff | Umgesetzt |
| [engine-pi.md](engine-pi.md) | Umschaltbare Engine: DSH oder Pi-Agent | Geplant |
| [regressionen-seit-aether.md](regressionen-seit-aether.md) | Fehler seit der Umbenennung zu Aether | Geplant |

## Bereits umgesetzt (vor diesem Ordner)

| Commit | Vorhaben |
|---|---|
| `6d9aa81` | Browser-Tool nutzt das eigene Chrome (`browser:"user"`), eine randlose Live-Vorschau |
| `4e4b442` | Subagenten-Aufrufe aus PTC wieder im Chat; Seitenleiste nur per Kopfzeilenknopf dauerhaft offen |
| `fd57055` | Passwortfelder für vom Nutzer verlangte Logins erlaubt |
| `300253d` | Freigegebene Chrome-Verbindung bleibt bestehen; Browser-Aktion `eval` |
| `b162da2` | Formatkorrektur, damit die CI auf `main` wieder läuft |
