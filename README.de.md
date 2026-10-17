# dsh-codex

[English](README.md)

Ein Plugin-Bundle für [DeepSeek Harness](https://www.deepseek.com/harness/)
(DSH). Es macht zwei Dinge:

- Es gleicht die DSH-Desktop-App optisch an die Codex-App von OpenAI an.
- Es ergänzt drei macOS-Werkzeuge mit sichtbarem Agent-Cursor:
  - `browser` steuert ein eigenes Chrome im Hintergrund.
  - `app` bedient eine Mac-App im Hintergrund über die Bedienungshilfen.
  - `computer` übernimmt den ganzen Bildschirm für Arbeit über mehrere Apps.

Ein schwebendes Mini-Fenster zeigt, was der Agent gerade tut. Es hat eine
Live-Vorschau und einen Stopp-Knopf.

> Keine Verbindung zu OpenAI oder DeepSeek. „Codex“ ist eine Marke von
> OpenAI. Das Projekt enthält keinen Code und keine Dateien aus Codex.
> Farbwerte und Maße stammen aus der Analyse der App. Cursor, Mini-Fenster
> und Animationen sind neu gezeichnet.

## Voraussetzungen

- **DeepSeek Harness Desktop 0.2.x.** Getestet mit 0.2.0-rc.2 und
  0.2.1-rc.2. DSH ist eine Vorschauversion. Die Plugin-Schnittstelle und
  die CSS-Ankerpunkte dieses Bundles können sich mit jedem Release ändern.
- **macOS 14 oder neuer auf Apple Silicon** für die Werkzeuge. Der Look
  funktioniert überall, wo die DSH-Weboberfläche läuft.
- **Xcode Command Line Tools** (`xcode-select --install`), um den Helfer zu
  bauen.
- **Für `browser`:** Google Chrome, Chromium, Brave oder Microsoft Edge.
- **Ein Modell mit Bild-Eingabe** liefert die besten Ergebnisse. Modelle
  ohne Bild-Eingabe arbeiten nur mit den Elementlisten.

## Installation

1. Repository klonen und den Helfer bauen. Das installiert
   `~/Applications/DSH Computer Use.app`:
   ```sh
   git clone https://github.com/NavidYSZ/dsh-codex.git
   cd dsh-codex
   helper/build.sh --install
   ```
   Das Skript signiert die App mit der ersten „Apple Development“-Identität
   oder mit `DSH_CUA_SIGN_IDENTITY`, falls gesetzt. Ohne beides signiert es
   ad hoc, und macOS fragt dann nach jedem Neubau erneut nach den
   Berechtigungen.
2. In **Systemeinstellungen → Datenschutz & Sicherheit** „DSH Computer Use“
   unter **Bedienungshilfen** und unter **Bildschirm- &
   Systemaudioaufnahme** erlauben.
3. DeepSeek Harness vollständig beenden. Das Desktop-Profil gehört der App,
   und die CLI braucht die Profilsperre.
4. Das Bundle mit der CLI hinzufügen, die in der App steckt:
   ```sh
   "/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh" \
     plugin --profile desktop add "$PWD"
   ```
5. DSH starten. Unter **Plugins** steht jetzt `@yousufzai/dsh-codex`.

**Nur der Look:** `computerUse.enabled`, `apps.enabled` und
`browser.enabled` auf `false` setzen (siehe
[Konfiguration](#konfiguration)). Dann braucht es weder den Helfer noch die
Berechtigungen.

**Web-Profil** (`dsh web`): `dsh plugin --profile web add <pfad>`. Unter
Linux sind `computer` und `app` aus. `browser` meldet sich nur an, wenn
`browser.chrome` auf ein Chrome-Programm zeigt. Diese Einrichtung ist
ungetestet.

**Aktualisieren:** Nach Änderungen an `client.js`, `index.js` oder `lib/`
DSH neu starten. Nach Änderungen am Helfer `helper/build.sh --install`
ausführen.

**Entfernen:**
1. DSH beenden.
2. `dsh plugin --profile desktop remove @yousufzai/dsh-codex` ausführen.
3. `~/Applications/DSH Computer Use.app` und
   `~/Library/Application Support/dsh-codex` löschen. Der Ordner enthält
   den Socket und das Browser-Profil.

## Was es macht

### Look (`client.js`)

- **Farben** über `ctx.theme.overrideTokens`:
  - neutrale Graustufen statt der bläulichen DSH-Palette;
  - Text `#1a1c1f`, Akzent `#339cff`, Hintergrund `#fff` / `#181818`.

  Hell, Dunkel und System bleiben wählbar.
- **Layout:**
  - 768 px Lesebreite;
  - dunkle Nutzer-Bubble;
  - flache Seitenleiste mit der Textmarke „DeepSeek“.
- **Composer:**
  - ohne Rand, mit weichem Schatten und 22 px Radius;
  - runder schwarzer Senden-Knopf;
  - Modellname in Textfarbe, Aufwand in Grau;
  - „Full access“ in Orange.

  Die Statistikzeile unter dem Composer bleibt blass, bis man darüberfährt.
- **Neue Sitzung:** ein grauer DeepSeek-Wal und die Frage „Woran sollen wir
  in ‹Arbeitsbereich› arbeiten?“ Der Name öffnet die Auswahl. Der Composer
  sitzt unten, Arbeitsbereich und Modus liegen in einer grauen Leiste
  hinter seiner Oberkante.
- **Chat-Zeilen für die drei Werkzeuge,** etwa „Browser · Klick „Suchen“ ·
  de.wikipedia.org“. Jede Zeile hat eine Screenshot-Vorschau, auf der der
  Agent-Cursor zum Ziel gleitet.

### Hintergrund-Browser (`lib/browser.js`, `lib/browser-tool.js`)

- **Aktionen:**
  - Seiten: `open`, `state`, `read`, `wait`;
  - Eingabe: `click`, `type`, `key`, `scroll`, `select`;
  - Navigation: `back`, `forward`, `reload`;
  - Tabs: `tabs`, `new_tab`, `switch_tab`, `close_tab`;
  - Fenster: `show`, `hide`.
- **Eigenes Chrome-Profil** unter
  `~/Library/Application Support/dsh-codex/browser-profile`, getrennt von
  deinem Chrome. Cookies bleiben zwischen den Aufrufen erhalten.
- **Headless.** Gesteuert über das DevTools-Protokoll per Pipe, ohne
  offenen Debug-Port. Du siehst keine Fenster und kein Dock-Symbol, und der
  Fokus wechselt nie.
- **Was das Modell sieht:** eine nummerierte Liste der sichtbaren
  Bedienelemente, etwa `[3] button "Suchen" @(x,y)`, und einen Screenshot.
  `read` liefert den Seitentext.
- **`show` und `hide`:** `show` holt den Browser als normales Fenster nach
  vorn, die Tabs bleiben erhalten. So kannst du dich anmelden. `hide`
  schickt ihn zurück.
- **Gesperrt:** Passwortfelder werden abgelehnt, Downloads sind blockiert.
- **Leerlauf:** Chrome beendet sich nach 15 Minuten ohne Nutzung.

### Hintergrund-Apps (`lib/app-tool.js`, `helper/Sources/AppControl.swift`)

- **Aktionen:** `state`, `click`, `type`, `set_value`, `key`, `scroll`,
  `menu`, `action`, `list_apps` und `show`.
- **So funktioniert es:** über die Bedienungshilfen (AXPress, AXValue,
  Menüleiste). Die App bleibt hinter deinen Fenstern, Mauszeiger und
  Tastatur bleiben unberührt.
- **Screenshots** zeigen nur das Fenster der App, auch wenn andere Fenster
  es verdecken.
- **Rückfall:** Wo die Bedienungshilfen keinen Weg bieten, schickt das
  Werkzeug Ereignisse direkt an den Prozess der App. Das kann die App nach
  vorn holen, und das Werkzeug meldet es dann.
- **Gesperrt:** Rechtsklick-Menüs gibt es im Hintergrund nicht,
  Passwortfelder werden abgelehnt.

### Mini-Fenster (`helper/Sources/Mini.swift`)

- **Was es zeigt:**
  - ein schwebendes Glasfenster, unten rechts, bis du es woandershin
    ziehst;
  - das App-Symbol, einen Titel wie „Browser · de.wikipedia.org“ oder
    „Rechner“ und den aktuellen Schritt;
  - eine Live-Vorschau von Browser oder App-Fenster, auf der der
    Agent-Cursor vor jedem Klick zum Ziel gleitet.
- **Stopp** hält den Agenten an. Die nächsten 20 Sekunden werden Aktionen
  abgelehnt, und das Modell soll dich fragen, wie es weitergeht.
- **Vorschau und Pfeil:** Ein Klick auf die Vorschau holt App oder Browser
  nach vorn. Der Pfeil klappt die Vorschau ein.
- **Verhalten:**
  - zeigt nach einigen ruhigen Sekunden „Fertig“ und blendet sich dann
    aus;
  - nimmt nie den Fokus;
  - erscheint weder auf Screenshots noch bei Bildschirmfreigaben.

### Computer Use im Vordergrund (`lib/computer-tool.js`, `helper/`)

- **Aktionen:** `screenshot`, `click`, `double_click`, `right_click`,
  `move`, `drag`, `scroll`, `type`, `key`, `open_app` und `wait`. Für Arbeit
  über mehrere Apps, Drag & Drop und Kontextmenüs.
- **Echte Maus:** Das Werkzeug bewegt deinen Mauszeiger. Ein
  klick-durchlässiges Overlay zeichnet den Agent-Cursor, und ein Banner
  zeigt „DeepSeek verwendet deinen Computer · Esc zum Abbrechen“.
- **Esc** stoppt nur diesen Modus. Im Hintergrund-Modus tippst du vielleicht
  gerade selbst irgendwo.

### Sperre

Der Helfer verweigert Klicks, Eingaben und App-Starts an mehreren Stellen:

- macOS-Berechtigungs-, Login- und Touch-ID-Dialoge;
- Systemeinstellungen;
- Schlüsselbundverwaltung;
- Passwort-Apps (Passwörter, 1Password, Bitwarden).

Das gilt für `computer` und `app`, egal was das Modell verlangt.

## Konfiguration

Eine Zeile in der `cordis.patch.yml` des Profils ergänzen. Für das
Desktop-Profil ist das `~/.dsh/profiles/desktop/cordis.patch.yml`.
Weggelassene Schlüssel behalten ihre Voreinstellung:

```yaml
- id: dsh-codex
  name: '@yousufzai/dsh-codex'
  config:
    computerUse:
      enabled: true          # false: kein Werkzeug "computer"
      locale: de             # de oder en; Standard: Sprache von macOS
      accent: '#339cff'      # Glow, Klick-Ring, Banner-Punkt, Mini-Cursor
      label: DeepSeek        # Namensschild am Cursor (showLabel)
      banner: DeepSeek verwendet deinen Computer
      escHint: Esc zum Abbrechen
      showLabel: false       # Codex zeigt kein Namensschild
      showGlow: false        # leuchtender Bildschirmrand
      showActivity: true     # „Klickt“, „Tippt …“ neben dem Cursor
      clickPulse: true       # Ring beim Klick
      maxWidth: 1280         # Screenshot-Breite in Pixeln = Koordinatenraum
      quality: 0.8           # JPEG-Qualität
      blockedApps: []        # zusätzliche gesperrte Bundle-IDs
    apps:
      enabled: true          # Werkzeug "app" (nur macOS)
    browser:
      enabled: true          # Werkzeug "browser"
      chrome: /Applications/Google Chrome.app/Contents/MacOS/Google Chrome
      width: 1280            # Viewport = Koordinatenraum
      height: 800
      idleMinutes: 15        # Chrome beendet sich danach
      quality: 0.7
    mini:
      doneAfter: 6           # ruhige Sekunden bis „Fertig“
      hideAfter: 24          # danach ausblenden
```

Die Texte in der Oberfläche folgen der Sprache des DSH-Fensters.
Aktivitäten, Banner und Mini-Fenster folgen `computerUse.locale`.

## Sicherheit

- **Die Werkzeuge handeln auf deinem echten Mac.** Was der Helfer
  verweigert, steht unter [Sperre](#sperre). Prüfe, was der Agent tut,
  besonders in einem Browser-Profil, in dem du angemeldet bist.
- **Daten verlassen deinen Mac.** Screenshots und Seitentexte gehen an
  deinen Modellanbieter, und DSH speichert die Screenshots als Anhänge der
  Sitzung.
- **Inhalte können Anweisungen enthalten.** Webseiten und App-Fenster
  können Prompt-Injection versuchen. Die Werkzeugbeschreibungen sagen dem
  Modell, diese Inhalte als nicht vertrauenswürdig zu behandeln und vor
  Senden, Kaufen, Löschen oder Veröffentlichen nachzufragen. Modelle halten
  sich nicht immer daran.
- **Anhalten:** Stopp im Mini-Fenster oder Esc im Vordergrund-Modus. Beides
  sperrt die Werkzeuge für 20 Sekunden.

## Entwicklung

| Pfad | Zweck |
|---|---|
| `client.js` | Browser-Modul: Tokens, Layout- und Composer-CSS, Startseiten-Überschrift, Chat-Zeilen |
| `index.js` | Host-Plugin: registriert `computer`, `app` und `browser`, speichert Screenshots als Anhänge |
| `lib/computer-tool.js` | Schema und Abbildung für den Vordergrund-Modus |
| `lib/app-tool.js` | Schema und Abbildung für Hintergrund-Apps |
| `lib/browser.js`, `lib/browser-tool.js` | Hintergrund-Chrome über DevTools-Pipe, Elementliste, Schema |
| `lib/cua-client.js` | Unix-Socket-Client; startet den Helfer bei Bedarf |
| `helper/` | Swift-Helfer: Overlay, Mini-Fenster, Screenshots über ScreenCaptureKit, Bedienungshilfen, Maus und Tastatur, Esc, Sperre |

`client.js` ist direkt im Browser-Modulformat von DSH geschrieben. Es gibt
keinen Build-Schritt.

**Socket des Helfers:** Der Helfer lauscht auf
`~/Library/Application Support/dsh-codex/cua.sock`, mit Rechten `0600` und
einer JSON-Zeile pro Anfrage. Zum Prüfen:

| Anfrage | Wirkung |
|---|---|
| `{"cmd":"status"}` | Meldet Version und Berechtigungen des Helfers |
| `{"cmd":"render_cursor","path":"/tmp/c.png"}` | Zeichnet den Cursor in mehreren Posen |
| `{"cmd":"guard_probe"}` | Zeigt, was die Sperre gerade blockieren würde |
| `{"cmd":"configure","mini":{"debugShareable":true}}` | Macht das Mini-Fenster für eigene Screenshots sichtbar |

**Chat-Zeilen ohne Sitzung ansehen:**
1. Im DSH-Fenster `localStorage['dsh-codex:preview'] = '1'` ausführen.
2. Neu laden.
3. `window.__dshCodexPreview` verwenden.

**Testen:** in einer eigenen Instanz mit eigenem `DSH_HOME` und ohne
API-Schlüssel in der Umgebung.

```sh
DSH_HOME=/tmp/dsh-test env -u DEEPSEEK_API_KEY dsh web --no-open --port 4719
```

DSH liest `DEEPSEEK_API_KEY` aus der Umgebung. Hat eine Test-Instanz den
Schlüssel und sind die Werkzeuge an, erreicht ein Test-Prompt ein echtes
Modell, und der Agent handelt auf deinem Mac. Die drei Werkzeuge im
Testprofil abschalten. Stattdessen direkt testen: `lib/*-tool.js` aus einem
Node-Skript importieren und auf harmlose Ziele wie den Rechner oder
example.com richten.

**Nach DSH-Updates:** Ein Teil des CSS greift über Modul-Klassen wie
`[class*="_composerHero"]` oder `[class*="_heroWorkspaceRow"]`. Diese nach
jedem DSH-Update prüfen.

## Grenzen

- `computer` bewegt die echte Maus. Für Hintergrundarbeit gibt es `app` und
  `browser`.
- `app` hängt vom Accessibility-Baum der jeweiligen App ab. Wo der dürftig
  ist, etwa bei Spielen und manchen Canvas-Apps, weicht das Werkzeug auf
  Koordinaten aus und auf Ereignisse, die die App nach vorn holen können.
- Apps im Hintergrund ignorieren ⌘-Kürzel oft. Dafür gibt es `menu`.
- Der Hintergrund-Browser ist nicht dein Chrome. Er kennt deine Logins
  erst, wenn du dich einmal über `show` selbst anmeldest.
- Manche Seiten erkennen Headless-Browser und zeigen Captchas. Die löst der
  Agent nicht.
- Die Breite der Seitenleiste lässt sich über die DSH-API nicht setzen.
  Codex' Icon-Leiste ganz links fehlt.
- Bei mehreren Monitoren arbeitet `computer` auf dem Bildschirm des letzten
  Screenshots. Das Mini-Fenster erscheint auf dem Bildschirm mit dem
  Mauszeiger oder dort, wo du es hingezogen hast.

## Lizenz

[MIT](LICENSE)
